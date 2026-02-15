use std::collections::{BTreeMap, HashMap};
use std::env::current_dir;
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::app::resolve_app_name;
use crate::build::{BuildCache, BuildExecutor};
use crate::commands::server;
use crate::config::{SecretsStore, ServerEntry, ServerTarget, ServersToml, TakoToml};
use crate::output;
use crate::runtime::detect_runtime;
use crate::ssh::{SshClient, SshConfig, SshError, upload_via_scp};
use crate::validation::{
    validate_full_config, validate_no_route_conflicts, validate_secrets_for_deployment,
};
use tako_core::{Command, Response};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TakoServerStatus {
    Ready,
    Missing,
    NotRunning,
}

/// Deployment configuration
#[derive(Clone)]
struct DeployConfig {
    app_name: String,
    version: String,
    archive_path: PathBuf,
    remote_base: String,
    routes: Vec<String>,
}

#[derive(Clone)]
struct ServerDeployTarget {
    name: String,
    server: ServerEntry,
    instances: u8,
    idle_timeout: u32,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct DeployArchiveManifest {
    app_name: String,
    environment: String,
    version: String,
    runtime: String,
    entry_point: String,
    env_vars: BTreeMap<String, String>,
    secret_names: Vec<String>,
}

struct ValidationResult {
    tako_config: TakoToml,
    servers: ServersToml,
    secrets: SecretsStore,
    env: String,
    runtime: Box<dyn crate::runtime::RuntimeAdapter>,
    warnings: Vec<String>,
}

impl DeployConfig {
    fn release_dir(&self) -> String {
        format!("{}/releases/{}", self.remote_base, self.version)
    }

    fn current_link(&self) -> String {
        format!("{}/current", self.remote_base)
    }

    fn shared_dir(&self) -> String {
        format!("{}/shared", self.remote_base)
    }
}

pub fn run(env: Option<&str>, assume_yes: bool) -> Result<(), Box<dyn std::error::Error>> {
    // Use tokio runtime for async SSH operations
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_async(env, assume_yes))
}

async fn run_async(
    requested_env: Option<&str>,
    assume_yes: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let project_dir = current_dir()?;

    let validation = output::with_spinner(
        "Validating configuration...",
        || -> Result<ValidationResult, String> {
            let tako_config = TakoToml::load_from_dir(&project_dir).map_err(|e| e.to_string())?;
            let servers = ServersToml::load().map_err(|e| e.to_string())?;
            let secrets = SecretsStore::load_from_dir(&project_dir).map_err(|e| e.to_string())?;

            let env = resolve_deploy_environment(requested_env, &tako_config)?;

            let config_result = validate_full_config(&tako_config, &servers);
            if config_result.has_errors() {
                return Err(format!(
                    "Configuration errors:\n  {}",
                    config_result.errors.join("\n  ")
                ));
            }
            let mut warnings = config_result.warnings.clone();

            let secrets_result = validate_secrets_for_deployment(&secrets, &env);
            if secrets_result.has_errors() {
                return Err(format!("Secret errors:\n  {}", secrets_result.errors.join("\n  ")));
            }
            warnings.extend(secrets_result.warnings.clone());

            let runtime = detect_runtime(&project_dir).ok_or_else(|| {
                "Could not detect runtime. Make sure you have a bun.lockb, bunfig.toml, or package.json with bun config.".to_string()
            })?;
            if !runtime.entry_point().exists() {
                return Err(format!(
                    "Entry point not found: {}",
                    runtime.entry_point().display()
                ));
            }

            Ok(ValidationResult {
                tako_config,
                servers,
                secrets,
                env,
                runtime,
                warnings,
            })
        },
    )?
    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    output::success("Validation complete");
    for warning in &validation.warnings {
        output::warning(&format!("Validation: {}", warning));
    }

    let ValidationResult {
        tako_config,
        mut servers,
        secrets,
        env,
        runtime,
        ..
    } = validation;

    confirm_production_deploy(&env, assume_yes)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    // Resolve target servers (explicit env mapping first, then production fallback).
    let server_names =
        resolve_deploy_server_names_with_setup(&tako_config, &mut servers, &env, &project_dir)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let use_per_server_spinners =
        should_use_per_server_spinners(server_names.len(), output::is_interactive());

    // Check all servers exist
    for server_name in &server_names {
        if !servers.contains(server_name) {
            return Err(format_server_not_found_error(server_name).into());
        }
    }
    let server_targets = resolve_deploy_server_targets(&servers, &server_names)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    output::section(&format_prepare_deploy_section(&env));

    let app_name = resolve_app_name(&project_dir).unwrap_or_else(|_| "app".to_string());
    let routes = required_env_routes(&tako_config, &env)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    output::success("Configuration valid");
    output::success(&format_runtime_summary(
        runtime.name(),
        runtime.version().as_deref(),
    ));
    output::success(&format_entry_point_summary(runtime.entry_point()));
    output::success(&format_servers_summary(&server_names));
    output::success(&format_server_targets_summary(&server_targets));

    // ===== Build =====
    output::section("Build");

    let executor = BuildExecutor::new(&project_dir);

    // Build artifacts must be produced under .tako/artifacts/app.
    let cache = BuildCache::new(project_dir.join(".tako/artifacts"));
    let _ = cache.clear();
    let artifact_input_dir = deploy_artifact_input_dir(&project_dir);

    // Run build command if configured
    if let Some(build_cmd) = runtime.build_command() {
        prepare_artifact_input_dir(&artifact_input_dir)
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
        let cmd_str = build_cmd.join(" ");
        let build_result =
            output::with_spinner(format!("Running build command ({})...", cmd_str), || {
                executor.run_build(&cmd_str)
            })?
            .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;

        if !build_result.success {
            return Err(format!("Build failed:\n{}", build_result.stderr).into());
        }
        output::success("Build command succeeded");
    } else {
        output::muted(&format!(
            "No build command configured; expecting prebuilt artifacts in {}",
            artifact_input_dir.display()
        ));
    }

    ensure_artifact_input_ready(&artifact_input_dir)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    merge_configured_asset_roots(&project_dir, &artifact_input_dir, &tako_config.tako.assets)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    if !tako_config.tako.assets.is_empty() {
        output::success("Merged [tako].assets into deploy public assets");
    }
    output::success(&format!(
        "Build artifacts: {}",
        artifact_input_dir.display()
    ));

    // Generate version string
    let version = executor.generate_version(None)?;
    output::success(&format!("Version: {}", version));

    let runtime_mode = deploy_runtime_mode(&env);
    let runtime_env_vars = runtime.env_vars(runtime_mode);

    let manifest = build_deploy_archive_manifest(
        &app_name,
        &env,
        &version,
        runtime.name(),
        runtime.entry_point(),
        tako_config.get_merged_vars(&env),
        runtime_env_vars,
        secrets.get_env(&env),
    );
    let package_dir = deploy_archive_package_dir(&project_dir);
    prepare_archive_package_dir(&package_dir)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    stage_archive_package(&artifact_input_dir, &package_dir, &manifest)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    output::success(&format!(
        "Archive manifest: {}",
        package_dir.join(DEPLOY_ARCHIVE_MANIFEST_FILE).display()
    ));

    // Create archive
    let archive_path = cache.cache_dir().join(format!("{}.tar.gz", version));
    let archive_size = output::with_spinner("Creating archive...", || {
        executor.create_archive(&package_dir, &archive_path, &[])
    })?
    .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;

    output::success(&format!(
        "Archive created: {} ({})",
        archive_path.display(),
        format_size(archive_size)
    ));

    // ===== Deploy =====
    output::section("Deploy");

    let deploy_config = Arc::new(DeployConfig {
        app_name: app_name.clone(),
        version: version.clone(),
        archive_path: archive_path.clone(),
        remote_base: format!("/opt/tako/apps/{}", app_name),
        routes,
    });

    let secrets = Arc::new(secrets);
    let env_str = env.clone();

    // Build per-server deploy targets (includes per-server scaling settings)
    let mut targets = Vec::new();
    for server_name in &server_names {
        let server = servers.get(server_name).unwrap().clone();
        targets.push(ServerDeployTarget {
            name: server_name.clone(),
            server,
            instances: tako_config.get_effective_instances(server_name),
            idle_timeout: tako_config.get_effective_idle_timeout(server_name),
        });
    }
    if targets.len() > 1 {
        output::step(&format_parallel_deploy_step(targets.len()));
    }

    // ===== tako-server preflight =====
    let mut missing_servers: Vec<String> = Vec::new();
    let mut not_running_servers: Vec<String> = Vec::new();

    let mut preflight_handles = Vec::new();
    for target in &targets {
        let server_name = target.name.clone();
        let server = target.server.clone();
        preflight_handles.push(tokio::spawn(async move {
            let status = check_tako_server(&server).await;
            (server_name, status)
        }));
    }

    for h in preflight_handles {
        let (server_name, status) = h
            .await
            .map_err(|e| format!("tako-server preflight task panic: {e}"))?;
        match status {
            Ok(TakoServerStatus::Ready) => {}
            Ok(TakoServerStatus::Missing) => missing_servers.push(server_name),
            Ok(TakoServerStatus::NotRunning) => not_running_servers.push(server_name),
            Err(e) => {
                return Err(format!("Failed to check tako-server on '{server_name}': {e}").into());
            }
        }
    }

    if !not_running_servers.is_empty() {
        return Err(format_tako_not_running_error(&not_running_servers).into());
    }

    if !missing_servers.is_empty() {
        return Err(format_tako_missing_error(&missing_servers).into());
    }

    // Spawn parallel deploy tasks
    let mut handles = Vec::new();
    for target in &targets {
        let server = target.server.clone();
        let server_name = target.name.clone();
        let deploy_config = deploy_config.clone();
        let secrets = secrets.clone();
        let env_str = env_str.clone();
        let instances = target.instances;
        let idle_timeout = target.idle_timeout;
        let use_spinner = use_per_server_spinners;
        let handle = tokio::spawn(async move {
            let result = deploy_to_server(
                &deploy_config,
                &server,
                &secrets,
                &env_str,
                instances,
                idle_timeout,
                use_spinner,
            )
            .await;
            (server_name, server, result)
        });
        handles.push(handle);
    }

    // Collect results
    let mut success_count = 0;
    let mut errors = Vec::new();

    for handle in handles {
        match handle.await {
            Ok((server_name, server, result)) => match result {
                Ok(()) => {
                    output::success(&format_server_deploy_success(&server_name, &server));
                    success_count += 1;
                }
                Err(e) => {
                    output::error(&format_server_deploy_failure(
                        &server_name,
                        &server,
                        &e.to_string(),
                    ));
                    errors.push(format!("{}: {}", server_name, e));
                }
            },
            Err(e) => {
                // Task panicked
                errors.push(format!("Task panic: {}", e));
            }
        }
    }

    // ===== Summary =====
    if errors.is_empty() {
        output::section("Summary");
        output::success(&format!("Deployed {} v{} to {}", app_name, version, env));

        let routes = required_env_routes(&tako_config, &env)
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
        output::step("Available at:");
        for route in routes {
            println!(
                "  {}",
                output::brand_secondary(format!("https://{}", route))
            );
        }

        Ok(())
    } else {
        output::section("Summary");
        output::warning(&format!(
            "Deployment partially failed: {}/{} servers succeeded",
            success_count,
            server_names.len()
        ));
        for err in &errors {
            output::error(err);
        }

        Err(format_partial_failure_error(errors.len()).into())
    }
}

fn resolve_deploy_environment(
    requested_env: Option<&str>,
    tako_config: &TakoToml,
) -> Result<String, String> {
    let env = if let Some(env) = requested_env {
        if env == "development" {
            return Err(
                "Environment 'development' is reserved for local development and cannot be deployed."
                    .to_string(),
            );
        }
        env.to_string()
    } else {
        "production".to_string()
    };

    if !tako_config.envs.contains_key(env.as_str()) {
        let available: Vec<String> = tako_config.envs.keys().cloned().collect();
        return Err(format_environment_not_found_error(&env, &available));
    }

    Ok(env)
}

fn required_env_routes(tako_config: &TakoToml, env: &str) -> Result<Vec<String>, String> {
    let routes = tako_config
        .get_routes(env)
        .ok_or_else(|| format!("Environment '{env}' has no routes configured"))?;
    if routes.is_empty() {
        return Err(format!(
            "Environment '{}' must define at least one route",
            env
        ));
    }
    Ok(routes)
}

fn should_confirm_production_deploy(env: &str, assume_yes: bool, interactive: bool) -> bool {
    env == "production" && !assume_yes && interactive
}

fn format_production_deploy_confirm_prompt() -> String {
    format!("Deploy to {} now?", output::emphasized("production"),)
}

fn format_production_deploy_confirm_hint() -> String {
    "Pass --yes/-y to skip this prompt.".to_string()
}

fn confirm_production_deploy(env: &str, assume_yes: bool) -> Result<(), String> {
    if !should_confirm_production_deploy(env, assume_yes, output::is_interactive()) {
        return Ok(());
    }

    output::warning(&format!(
        "You are deploying to {}.",
        output::emphasized("production")
    ));
    let hint = format_production_deploy_confirm_hint();
    let confirmed = output::confirm_with_description(
        &format_production_deploy_confirm_prompt(),
        Some(&hint),
        false,
    )
    .map_err(|e| format!("Failed to read confirmation: {}", e))?;
    if confirmed {
        Ok(())
    } else {
        Err("Deployment cancelled.".to_string())
    }
}

fn resolve_deploy_server_names(
    tako_config: &TakoToml,
    servers: &ServersToml,
    env: &str,
    confirmation: Option<bool>,
) -> Result<Vec<String>, String> {
    let server_names: Vec<String> = tako_config
        .get_servers_for_env(env)
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    if !server_names.is_empty() {
        return Ok(server_names);
    }

    if env == "production" && servers.len() == 1 {
        let server_name = servers.names().into_iter().next().unwrap_or("<server>");

        let confirmed = match confirmation {
            Some(value) => value,
            None => confirm_single_server_production_fallback(server_name)?,
        };

        if confirmed {
            return Ok(vec![server_name.to_string()]);
        }

        return Err(
            "Deployment cancelled. Add [servers.<name>] with env = \"production\" to tako.toml, or rerun with --env <name>."
                .to_string(),
        );
    }

    if servers.is_empty() {
        return Err(format_no_global_servers_error());
    }

    Err(format_no_servers_for_env_error(env))
}

fn confirm_single_server_production_fallback(server_name: &str) -> Result<bool, String> {
    if !output::is_interactive() {
        return Ok(true);
    }

    output::warning(&format!(
        "No server is mapped for {} in tako.toml.",
        output::emphasized("production")
    ));
    output::muted("Add this mapping manually:");
    for line in format_production_mapping_example(server_name).lines() {
        output::muted(&format!("  {}", line));
    }
    output::muted("If you choose 'Yes', tako will add this mapping to tako.toml for you.");

    output::confirm(
        &format_single_server_production_confirm_prompt(server_name),
        true,
    )
    .map_err(|e| format!("Failed to read confirmation: {}", e))
}

fn format_production_mapping_example(server_name: &str) -> String {
    format!("[servers.{server_name}]\nenv = \"production\"")
}

fn format_single_server_production_confirm_prompt(server_name: &str) -> String {
    format!(
        "Deploy to the only configured server ({}) and save this mapping to tako.toml?",
        output::emphasized(server_name)
    )
}

async fn resolve_deploy_server_names_with_setup(
    tako_config: &TakoToml,
    servers: &mut ServersToml,
    env: &str,
    project_dir: &Path,
) -> Result<Vec<String>, String> {
    let has_explicit_env_mapping = !tako_config.get_servers_for_env(env).is_empty();

    match resolve_deploy_server_names(tako_config, servers, env, None) {
        Ok(names) => {
            if env == "production" && !has_explicit_env_mapping && names.len() == 1 {
                persist_server_env_mapping(project_dir, &names[0], env)?;
                output::success(&format!(
                    "Mapped server {} to {} in tako.toml",
                    output::emphasized(&names[0]),
                    output::emphasized(env)
                ));
            }
            Ok(names)
        }
        Err(original_error) => {
            if env != "production" {
                return Err(original_error);
            }

            if servers.is_empty() {
                let added = server::prompt_to_add_server(
                    "No servers have been added. Deployment needs at least one production server.",
                )
                .await
                .map_err(|e| format!("Failed to run server setup: {}", e))?;

                if added.is_none() {
                    return Err(original_error);
                }

                *servers = ServersToml::load().map_err(|e| e.to_string())?;
            }

            if servers.is_empty() {
                return Err(format_no_global_servers_error());
            }

            let selected_server = if servers.len() == 1 {
                let server_name = servers
                    .names()
                    .into_iter()
                    .next()
                    .unwrap_or("<server>")
                    .to_string();
                choose_or_add_production_server_after_single_fallback(servers, &server_name, None)
                    .await?
            } else {
                select_production_server_for_mapping(servers)?
            };

            persist_server_env_mapping(project_dir, &selected_server, env)?;
            output::success(&format!(
                "Mapped server {} to {} in tako.toml",
                output::emphasized(&selected_server),
                output::emphasized(env)
            ));
            Ok(vec![selected_server])
        }
    }
}

async fn choose_or_add_production_server_after_single_fallback(
    servers: &mut ServersToml,
    server_name: &str,
    confirmation: Option<bool>,
) -> Result<String, String> {
    let confirmed = match confirmation {
        Some(value) => value,
        None => confirm_single_server_production_fallback(server_name)?,
    };
    if confirmed {
        return Ok(server_name.to_string());
    }

    let added = server::prompt_to_add_server(&format_declined_single_server_reason(server_name))
        .await
        .map_err(|e| format!("Failed to run server setup: {}", e))?;

    let Some(added_server_name) = added else {
        return Err(
            "Deployment cancelled. Add [servers.<name>] with env = \"production\" to tako.toml, or rerun with --env <name>."
                .to_string(),
        );
    };

    *servers = ServersToml::load().map_err(|e| e.to_string())?;
    if !servers.contains(&added_server_name) {
        return Err(format_server_not_found_error(&added_server_name));
    }

    Ok(added_server_name)
}

fn format_declined_single_server_reason(server_name: &str) -> String {
    format!(
        "Skipped using {} for {}. Add a different server now and use it for production deploy.",
        output::emphasized(server_name),
        output::emphasized("production")
    )
}

fn select_production_server_for_mapping(servers: &ServersToml) -> Result<String, String> {
    if !output::is_interactive() {
        return Err(format_no_servers_for_env_error("production"));
    }

    let mut names: Vec<&str> = servers.names();
    names.sort_unstable();

    let options = names
        .into_iter()
        .filter_map(|name| {
            servers
                .get(name)
                .map(|entry| (format_server_mapping_option(name, entry), name.to_string()))
        })
        .collect::<Vec<_>>();

    output::select(
        "Select server for production deploy",
        Some("No [servers.*] mapping was found. We will save your selection to tako.toml."),
        options,
    )
    .map_err(|e| format!("Failed to read selection: {}", e))
}

fn format_prepare_deploy_section(env: &str) -> String {
    format!("Preparing deployment for {}", output::emphasized(env))
}

fn format_parallel_deploy_step(server_count: usize) -> String {
    format!("Deploying to {} server(s) in parallel", server_count)
}

fn format_server_deploy_target(name: &str, entry: &ServerEntry) -> String {
    format!("{name} (tako@{}:{})", entry.host, entry.port)
}

fn format_server_deploy_success(name: &str, entry: &ServerEntry) -> String {
    format!(
        "{} deployed successfully",
        format_server_deploy_target(name, entry)
    )
}

fn format_server_deploy_failure(name: &str, entry: &ServerEntry, error: &str) -> String {
    format!(
        "{} deploy failed: {}",
        format_server_deploy_target(name, entry),
        error
    )
}

fn format_server_mapping_option(name: &str, entry: &ServerEntry) -> String {
    match entry.description.as_deref().map(str::trim) {
        Some(description) if !description.is_empty() => {
            format!("{name} ({description})  tako@{}:{}", entry.host, entry.port)
        }
        _ => format!("{name}  tako@{}:{}", entry.host, entry.port),
    }
}

fn persist_server_env_mapping(
    project_dir: &Path,
    server_name: &str,
    env: &str,
) -> Result<(), String> {
    TakoToml::upsert_server_env_in_dir(project_dir, server_name, env).map_err(|e| {
        format!("Failed to update tako.toml with [servers.{server_name}] env = \"{env}\": {e}")
    })
}

fn format_environment_not_found_error(env: &str, available: &[String]) -> String {
    let available_text = if available.is_empty() {
        "(none)".to_string()
    } else {
        available.join(", ")
    };
    format!(
        "Environment '{}' not found. Available: {}",
        env, available_text
    )
}

fn format_no_servers_for_env_error(env: &str) -> String {
    format!(
        "No servers configured for environment '{}'. Add [servers.<name>] with env = \"{}\" to tako.toml.",
        env, env
    )
}

fn format_no_global_servers_error() -> String {
    "No servers have been added. Run 'tako servers add <host>' first, then map it in tako.toml with [servers.<name>] env = \"production\".".to_string()
}

fn format_server_not_found_error(server_name: &str) -> String {
    format!(
        "Server '{}' not found in ~/.tako/config.toml [[servers]]. Run 'tako servers add --name {} <host>'.",
        server_name, server_name
    )
}

fn format_tako_not_running_error(server_names: &[String]) -> String {
    format!(
        "tako-server is installed but not running on: {} (start it as root, then re-run deploy)",
        server_names.join(", ")
    )
}

fn format_tako_missing_error(server_names: &[String]) -> String {
    format!(
        "tako-server is not installed on: {} (install it as root; see scripts/install-tako-server.sh)",
        server_names.join(", ")
    )
}

fn format_partial_failure_error(failed_servers: usize) -> String {
    format!("{} server(s) failed", failed_servers)
}

const DEPLOY_ARTIFACT_INPUT_SUBDIR: &str = ".tako/artifacts/app";
const DEPLOY_ARCHIVE_PACKAGE_SUBDIR: &str = ".tako/artifacts/package";
const DEPLOY_ARCHIVE_MANIFEST_FILE: &str = "app.json";

fn deploy_artifact_input_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(DEPLOY_ARTIFACT_INPUT_SUBDIR)
}

fn deploy_archive_package_dir(project_dir: &Path) -> PathBuf {
    project_dir.join(DEPLOY_ARCHIVE_PACKAGE_SUBDIR)
}

fn prepare_artifact_input_dir(artifact_input_dir: &Path) -> Result<(), String> {
    if artifact_input_dir.exists() {
        std::fs::remove_dir_all(artifact_input_dir).map_err(|e| {
            format!(
                "Failed to clear artifact directory {}: {}",
                artifact_input_dir.display(),
                e
            )
        })?;
    }

    std::fs::create_dir_all(artifact_input_dir).map_err(|e| {
        format!(
            "Failed to create artifact directory {}: {}",
            artifact_input_dir.display(),
            e
        )
    })?;

    Ok(())
}

fn ensure_artifact_input_ready(artifact_input_dir: &Path) -> Result<(), String> {
    if !artifact_input_dir.is_dir() {
        return Err(format!(
            "Artifact directory {} must contain build artifacts for deploy, but it does not exist.",
            artifact_input_dir.display()
        ));
    }

    if !directory_contains_any_file(artifact_input_dir)? {
        return Err(format!(
            "Artifact directory {} is empty and must contain build artifacts for deploy.",
            artifact_input_dir.display()
        ));
    }

    Ok(())
}

fn merge_configured_asset_roots(
    project_dir: &Path,
    artifact_input_dir: &Path,
    asset_roots: &[String],
) -> Result<(), String> {
    if asset_roots.is_empty() {
        return Ok(());
    }

    let merge_target_dir = resolve_asset_merge_target_dir(artifact_input_dir)?;
    std::fs::create_dir_all(&merge_target_dir).map_err(|e| {
        format!(
            "Failed to create asset merge target directory {}: {}",
            merge_target_dir.display(),
            e
        )
    })?;

    for asset_root in asset_roots {
        let source_dir = resolve_project_asset_root(project_dir, asset_root)?;
        merge_directory_contents_without_overwrite(&source_dir, &merge_target_dir)?;
    }

    Ok(())
}

fn resolve_asset_merge_target_dir(artifact_input_dir: &Path) -> Result<PathBuf, String> {
    let static_dir = artifact_input_dir.join("static");
    if static_dir.exists() {
        if !static_dir.is_dir() {
            return Err(format!(
                "Deploy artifact path {} exists but is not a directory",
                static_dir.display()
            ));
        }
        return Ok(static_dir);
    }

    let public_dir = artifact_input_dir.join("public");
    if public_dir.exists() && !public_dir.is_dir() {
        return Err(format!(
            "Deploy artifact path {} exists but is not a directory",
            public_dir.display()
        ));
    }

    Ok(public_dir)
}

fn resolve_project_asset_root(project_dir: &Path, asset_root: &str) -> Result<PathBuf, String> {
    let trimmed = asset_root.trim();
    if trimmed.is_empty() {
        return Err("Configured [tako].assets entry cannot be empty".to_string());
    }

    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(format!(
            "Configured [tako].assets entry '{}' must be relative to project root",
            asset_root
        ));
    }

    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(format!(
            "Configured [tako].assets entry '{}' must not contain '..'",
            asset_root
        ));
    }

    let source_dir = project_dir.join(path);
    if !source_dir.exists() {
        return Err(format!(
            "Configured asset directory '{}' does not exist under {}",
            asset_root,
            project_dir.display()
        ));
    }
    if !source_dir.is_dir() {
        return Err(format!(
            "Configured asset path '{}' is not a directory",
            asset_root
        ));
    }

    Ok(source_dir)
}

fn merge_directory_contents_without_overwrite(
    source_dir: &Path,
    dest_dir: &Path,
) -> Result<(), String> {
    let entries = std::fs::read_dir(source_dir).map_err(|e| {
        format!(
            "Failed to read source asset directory {}: {}",
            source_dir.display(),
            e
        )
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| {
            format!(
                "Failed to read entry in source asset directory {}: {}",
                source_dir.display(),
                e
            )
        })?;
        let source_path = entry.path();
        let dest_path = dest_dir.join(entry.file_name());
        let file_type = entry.file_type().map_err(|e| {
            format!(
                "Failed to inspect source asset path {}: {}",
                source_path.display(),
                e
            )
        })?;

        if file_type.is_dir() {
            std::fs::create_dir_all(&dest_path).map_err(|e| {
                format!(
                    "Failed to create destination asset directory {}: {}",
                    dest_path.display(),
                    e
                )
            })?;
            merge_directory_contents_without_overwrite(&source_path, &dest_path)?;
        } else if file_type.is_file() {
            if dest_path.exists() {
                continue;
            }
            std::fs::copy(&source_path, &dest_path).map_err(|e| {
                format!(
                    "Failed to copy asset file {} to {}: {}",
                    source_path.display(),
                    dest_path.display(),
                    e
                )
            })?;
        } else if file_type.is_symlink() {
            return Err(format!(
                "Configured assets cannot contain symbolic links: {}",
                source_path.display()
            ));
        }
    }

    Ok(())
}

fn directory_contains_any_file(dir: &Path) -> Result<bool, String> {
    let entries = std::fs::read_dir(dir)
        .map_err(|e| format!("Failed to read artifact directory {}: {}", dir.display(), e))?;

    for entry in entries {
        let entry = entry.map_err(|e| {
            format!(
                "Failed to read an entry in artifact directory {}: {}",
                dir.display(),
                e
            )
        })?;

        let file_type = entry.file_type().map_err(|e| {
            format!(
                "Failed to inspect artifact entry {}: {}",
                entry.path().display(),
                e
            )
        })?;
        if file_type.is_file() || file_type.is_symlink() {
            return Ok(true);
        }
        if file_type.is_dir() && directory_contains_any_file(&entry.path())? {
            return Ok(true);
        }
    }

    Ok(false)
}

fn build_deploy_archive_manifest(
    app_name: &str,
    environment: &str,
    version: &str,
    runtime_name: &str,
    entry_point: &Path,
    app_env_vars: HashMap<String, String>,
    runtime_env_vars: HashMap<String, String>,
    env_secrets: Option<&HashMap<String, String>>,
) -> DeployArchiveManifest {
    let mut secret_names = env_secrets
        .map(|map| map.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    secret_names.sort();

    DeployArchiveManifest {
        app_name: app_name.to_string(),
        environment: environment.to_string(),
        version: version.to_string(),
        runtime: runtime_name.to_string(),
        entry_point: entry_point.display().to_string(),
        env_vars: build_manifest_env_vars(
            app_env_vars,
            runtime_env_vars,
            environment,
            runtime_name,
        ),
        secret_names,
    }
}

fn build_manifest_env_vars(
    app_env_vars: HashMap<String, String>,
    mut runtime_env_vars: HashMap<String, String>,
    environment: &str,
    runtime_name: &str,
) -> BTreeMap<String, String> {
    if runtime_name == "bun" {
        // For Bun deploys, tie Node/Bun env conventions to the selected Tako env.
        if runtime_env_vars.contains_key("NODE_ENV") {
            runtime_env_vars.insert("NODE_ENV".to_string(), environment.to_string());
        }
        if runtime_env_vars.contains_key("BUN_ENV") {
            runtime_env_vars.insert("BUN_ENV".to_string(), environment.to_string());
        }
    }

    let mut merged = BTreeMap::new();
    for (key, value) in app_env_vars {
        merged.insert(key, value);
    }
    for (key, value) in runtime_env_vars {
        merged.insert(key, value);
    }
    merged.insert("TAKO_ENV".to_string(), environment.to_string());
    merged
}

fn deploy_runtime_mode(environment: &str) -> crate::runtime::RuntimeMode {
    if environment == "development" {
        crate::runtime::RuntimeMode::Development
    } else {
        crate::runtime::RuntimeMode::Production
    }
}

fn prepare_archive_package_dir(package_dir: &Path) -> Result<(), String> {
    if package_dir.exists() {
        std::fs::remove_dir_all(package_dir).map_err(|e| {
            format!(
                "Failed to clear archive package directory {}: {}",
                package_dir.display(),
                e
            )
        })?;
    }
    std::fs::create_dir_all(package_dir).map_err(|e| {
        format!(
            "Failed to create archive package directory {}: {}",
            package_dir.display(),
            e
        )
    })?;
    Ok(())
}

fn stage_archive_package(
    artifact_input_dir: &Path,
    package_dir: &Path,
    manifest: &DeployArchiveManifest,
) -> Result<(), String> {
    let packaged_artifacts_dir = package_dir.join("artifacts");
    std::fs::create_dir_all(&packaged_artifacts_dir).map_err(|e| {
        format!(
            "Failed to create packaged artifacts directory {}: {}",
            packaged_artifacts_dir.display(),
            e
        )
    })?;
    copy_directory_contents(artifact_input_dir, &packaged_artifacts_dir)?;

    let manifest_path = package_dir.join(DEPLOY_ARCHIVE_MANIFEST_FILE);
    let manifest_json = serde_json::to_string_pretty(manifest)
        .map_err(|e| format!("Failed to serialize deploy manifest: {}", e))?;
    std::fs::write(&manifest_path, manifest_json).map_err(|e| {
        format!(
            "Failed to write deploy manifest {}: {}",
            manifest_path.display(),
            e
        )
    })?;
    Ok(())
}

fn copy_directory_contents(source_dir: &Path, dest_dir: &Path) -> Result<(), String> {
    let entries = std::fs::read_dir(source_dir).map_err(|e| {
        format!(
            "Failed to read source artifact directory {}: {}",
            source_dir.display(),
            e
        )
    })?;

    for entry in entries {
        let entry = entry.map_err(|e| {
            format!(
                "Failed to read entry in source artifact directory {}: {}",
                source_dir.display(),
                e
            )
        })?;
        let source_path = entry.path();
        let dest_path = dest_dir.join(entry.file_name());
        let file_type = entry.file_type().map_err(|e| {
            format!(
                "Failed to inspect source artifact path {}: {}",
                source_path.display(),
                e
            )
        })?;

        if file_type.is_dir() {
            std::fs::create_dir_all(&dest_path).map_err(|e| {
                format!(
                    "Failed to create destination directory {}: {}",
                    dest_path.display(),
                    e
                )
            })?;
            copy_directory_contents(&source_path, &dest_path)?;
        } else if file_type.is_file() {
            std::fs::copy(&source_path, &dest_path).map_err(|e| {
                format!(
                    "Failed to copy artifact file {} to {}: {}",
                    source_path.display(),
                    dest_path.display(),
                    e
                )
            })?;
        } else if file_type.is_symlink() {
            return Err(format!(
                "Deploy artifacts cannot contain symbolic links: {}",
                source_path.display()
            ));
        }
    }

    Ok(())
}

fn format_runtime_summary(runtime_name: &str, version: Option<&str>) -> String {
    match version.map(str::trim) {
        Some(version) if !version.is_empty() => {
            format!("Runtime: {} ({})", runtime_name, version)
        }
        _ => format!("Runtime: {}", runtime_name),
    }
}

fn format_entry_point_summary(entry_point: &Path) -> String {
    format!("Entry point: {}", entry_point.display())
}

fn format_servers_summary(server_names: &[String]) -> String {
    format!("Servers: {}", server_names.join(", "))
}

fn resolve_deploy_server_targets(
    servers: &ServersToml,
    server_names: &[String],
) -> Result<Vec<(String, ServerTarget)>, String> {
    let mut resolved = Vec::with_capacity(server_names.len());
    let mut missing = Vec::new();
    let mut invalid = Vec::new();

    for server_name in server_names {
        let Some(raw_target) = servers.get_target(server_name) else {
            missing.push(server_name.clone());
            continue;
        };

        match ServerTarget::normalized(&raw_target.arch, &raw_target.libc) {
            Ok(target) => resolved.push((server_name.clone(), target)),
            Err(err) => invalid.push(format!(
                "{} (arch='{}', libc='{}': {})",
                server_name, raw_target.arch, raw_target.libc, err
            )),
        }
    }

    if !missing.is_empty() || !invalid.is_empty() {
        return Err(format_server_target_metadata_error(&missing, &invalid));
    }

    Ok(resolved)
}

fn format_server_target_metadata_error(missing: &[String], invalid: &[String]) -> String {
    let mut details = Vec::new();
    if !missing.is_empty() {
        details.push(format!("missing targets for: {}", missing.join(", ")));
    }
    if !invalid.is_empty() {
        details.push(format!("invalid targets for: {}", invalid.join(", ")));
    }

    format!(
        "Deploy target metadata check failed: {}. Remove and add each affected server again (`tako servers rm <name>` then `tako servers add --name <name> <host>`). Deploy does not probe server targets.",
        details.join("; ")
    )
}

fn format_server_targets_summary(server_targets: &[(String, ServerTarget)]) -> String {
    let mut labels = server_targets
        .iter()
        .map(|(_, target)| target.label())
        .collect::<Vec<_>>();
    labels.sort();
    labels.dedup();
    format!("Server targets: {}", labels.join(", "))
}

fn parse_existing_routes_response(
    response: Response,
) -> Result<Vec<(String, Vec<String>)>, String> {
    match response {
        Response::Ok { data } => Ok(data
            .get("routes")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|item| {
                        let app = item.get("app")?.as_str()?.to_string();
                        let routes = item
                            .get("routes")
                            .and_then(|r| r.as_array())
                            .map(|r| {
                                r.iter()
                                    .filter_map(|x| x.as_str().map(|s| s.to_string()))
                                    .collect::<Vec<_>>()
                            })
                            .unwrap_or_default();
                        Some((app, routes))
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()),
        Response::Error { message } => Err(format!("tako-server error (routes): {}", message)),
    }
}

fn build_env_file_contents(
    version: &str,
    env_secrets: Option<&std::collections::HashMap<String, String>>,
) -> String {
    let mut env_content = String::new();
    env_content.push_str(&format!("TAKO_BUILD=\"{}\"\n", version));

    if let Some(secrets) = env_secrets {
        for (key, value) in secrets {
            let escaped = value.replace("\\", "\\\\").replace("\"", "\\\"");
            env_content.push_str(&format!("{}=\"{}\"\n", key, escaped));
        }
    }

    env_content
}

fn deploy_response_has_error(response: &str) -> bool {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(response) {
        if value.get("status").and_then(|s| s.as_str()) == Some("error") {
            return true;
        }
        return value.get("error").is_some();
    }
    response.contains("\"error\"")
}

const DEPLOY_DISK_CHECK_PATH: &str = "/opt/tako";
const DEPLOY_DISK_SPACE_MULTIPLIER: u64 = 3;
const DEPLOY_DISK_SPACE_HEADROOM_BYTES: u64 = 64 * 1024 * 1024;

fn shell_single_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn required_remote_free_bytes(archive_size_bytes: u64) -> u64 {
    archive_size_bytes
        .saturating_mul(DEPLOY_DISK_SPACE_MULTIPLIER)
        .saturating_add(DEPLOY_DISK_SPACE_HEADROOM_BYTES)
}

fn parse_df_available_kb(stdout: &str) -> Result<u64, String> {
    let line = stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| "empty df output".to_string())?;
    line.parse::<u64>()
        .map_err(|_| format!("unexpected df output: '{line}'"))
}

fn format_insufficient_disk_space_error(
    required_bytes: u64,
    available_bytes: u64,
    archive_size_bytes: u64,
) -> String {
    format!(
        "Insufficient disk space under {}. Archive size: {}. Required free space: {}. Available free space: {}. Free space under {} and retry deploy.",
        DEPLOY_DISK_CHECK_PATH,
        format_size(archive_size_bytes),
        format_size(required_bytes),
        format_size(available_bytes),
        DEPLOY_DISK_CHECK_PATH
    )
}

fn cleanup_partial_release_command(release_dir: &str) -> String {
    format!("rm -rf {}", shell_single_quote(release_dir))
}

async fn ensure_remote_disk_space(
    ssh: &SshClient,
    archive_size_bytes: u64,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let required_bytes = required_remote_free_bytes(archive_size_bytes);
    let cmd = format!(
        "df -Pk {} | awk 'NR==2 {{print $4}}'",
        shell_single_quote(DEPLOY_DISK_CHECK_PATH)
    );
    let output = ssh.exec(&cmd).await?;
    if !output.success() {
        return Err(format!(
            "Failed to check free disk space under {}: {}",
            DEPLOY_DISK_CHECK_PATH,
            output.combined().trim()
        )
        .into());
    }

    let available_kb = parse_df_available_kb(&output.stdout)
        .map_err(|e| format!("Failed to parse free disk space: {}", e))?;
    let available_bytes = available_kb.saturating_mul(1024);
    if available_bytes < required_bytes {
        return Err(format_insufficient_disk_space_error(
            required_bytes,
            available_bytes,
            archive_size_bytes,
        )
        .into());
    }

    Ok(())
}

async fn remote_directory_exists(
    ssh: &SshClient,
    path: &str,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let quoted = shell_single_quote(path);
    let cmd = format!("if [ -d {quoted} ]; then echo yes; else echo no; fi");
    let output = ssh.exec(&cmd).await?;
    if !output.success() {
        return Err(format!(
            "Failed to check remote directory existence for {}: {}",
            path,
            output.combined().trim()
        )
        .into());
    }
    Ok(output.stdout.trim() == "yes")
}

async fn cleanup_partial_release(
    ssh: &SshClient,
    release_dir: &str,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    ssh.exec_checked(&cleanup_partial_release_command(release_dir))
        .await?;
    Ok(())
}

fn should_use_per_server_spinners(server_count: usize, interactive: bool) -> bool {
    interactive && server_count == 1
}

async fn run_deploy_step<T, E, Fut>(
    label: &'static str,
    use_spinner: bool,
    work: Fut,
) -> Result<T, Box<dyn std::error::Error + Send + Sync>>
where
    Fut: Future<Output = Result<T, E>> + Send,
    T: Send,
    E: Send + Into<Box<dyn std::error::Error + Send + Sync>>,
{
    if use_spinner {
        let result = output::with_spinner_async(label, work)
            .await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
        result.map_err(Into::into)
    } else {
        output::step(label);
        work.await.map_err(Into::into)
    }
}

/// Deploy to a single server
async fn deploy_to_server(
    config: &DeployConfig,
    server: &ServerEntry,
    secrets: &SecretsStore,
    env: &str,
    instances: u8,
    idle_timeout: u32,
    use_spinner: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ssh_config = SshConfig::from_server(&server.host, server.port);
    let ssh_keys_dir = ssh_config.keys_directory();
    let mut ssh = SshClient::new(ssh_config);
    run_deploy_step("Connecting...", use_spinner, ssh.connect()).await?;
    let archive_size_bytes = std::fs::metadata(&config.archive_path)?.len();

    // Server-side deploy lock (best-effort). This prevents concurrent deploys of the same app.
    let lock_dir = format!("{}/.deploy_lock", config.remote_base);
    let lock_cmd = format!(
        "mkdir -p {} && mkdir {} 2>/dev/null && echo ok || echo locked",
        config.remote_base, lock_dir
    );
    let lock_check =
        run_deploy_step("Acquiring deploy lock...", use_spinner, ssh.exec(&lock_cmd)).await?;
    if !lock_check.stdout.trim().contains("ok") {
        let _ = ssh.disconnect().await;
        return Err(format!("deploy lock already held at {}", lock_dir).into());
    }

    run_deploy_step(
        "Checking remote disk space...",
        use_spinner,
        ensure_remote_disk_space(&ssh, archive_size_bytes),
    )
    .await?;

    let release_dir = config.release_dir();
    let release_dir_preexisted = remote_directory_exists(&ssh, &release_dir).await?;

    let result = async {
        // Check if tako-server is installed.
        let installed =
            run_deploy_step("Checking tako-server...", use_spinner, ssh.is_tako_installed())
                .await?;
        if !installed {
            return Err(
                "tako-server is not installed on this server. Install it as root (see scripts/install-tako-server.sh)."
                    .into(),
            );
        }

        // Ensure the service is running before socket commands.
        run_deploy_step(
            "Checking tako-server status...",
            use_spinner,
            ensure_tako_running(&mut ssh),
        )
        .await?;

        // Route conflict validation (best-effort against current tako-server state)
        let existing = run_deploy_step("Checking route conflicts...", use_spinner, async {
            parse_existing_routes_response(ssh.tako_routes().await?)
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })
        })
        .await?;

        validate_no_route_conflicts(&existing, &config.app_name, &config.routes)
            .map_err(|e| format!("Route conflict: {}", e))?;

        // Create directories.
        run_deploy_step("Creating directories...", use_spinner, async {
            ssh.mkdir(&release_dir).await?;
            ssh.mkdir(&config.shared_dir()).await?;
            Ok::<(), SshError>(())
        })
        .await?;

        // Upload archive.
        let remote_archive = format!("{}/release.tar.gz", release_dir);
        run_deploy_step(
            "Uploading archive...",
            use_spinner,
            upload_via_scp(
                &config.archive_path,
                &server.host,
                server.port,
                &remote_archive,
                &ssh_keys_dir,
            ),
        )
        .await?;

        // Extract archive and unpack payload/artifacts to release root.
        let extract_cmd = format!(
            "cd {} && tar -xzf release.tar.gz && rm release.tar.gz && if [ -d artifacts ]; then cp -R artifacts/. . && rm -rf artifacts; fi",
            release_dir
        );
        run_deploy_step(
            "Extracting archive payload...",
            use_spinner,
            ssh.exec_checked(&extract_cmd),
        )
        .await?;

        // Symlink shared directories (logs, etc.).
        let shared_link_cmd = format!(
            "mkdir -p {}/logs && ln -sfn {}/logs {}/logs",
            config.shared_dir(),
            config.shared_dir(),
            release_dir
        );
        run_deploy_step(
            "Linking shared directories...",
            use_spinner,
            ssh.exec_checked(&shared_link_cmd),
        )
        .await?;

        // Write .env file with TAKO_BUILD version and secrets.
        run_deploy_step("Writing .env...", use_spinner, async {
            let env_content = build_env_file_contents(&config.version, secrets.get_env(env));
            let env_file = format!("{}/.env", release_dir);
            let encoded = base64::Engine::encode(
                &base64::engine::general_purpose::STANDARD,
                env_content.as_bytes(),
            );
            let env_cmd = format!(
                "echo '{}' | base64 -d > {} && chmod 600 {}",
                encoded, env_file, env_file
            );
            ssh.exec_checked(&env_cmd).await?;
            Ok::<(), SshError>(())
        })
        .await?;

        // Send deploy command to tako-server.
        let cmd = Command::Deploy {
            app: config.app_name.clone(),
            version: config.version.clone(),
            path: release_dir.clone(),
            routes: config.routes.clone(),
            instances,
            idle_timeout,
        };
        let json = serde_json::to_string(&cmd)?;
        let response =
            run_deploy_step("Notifying tako-server...", use_spinner, ssh.tako_command(&json))
                .await?;

        // Parse response.
        if deploy_response_has_error(&response) {
            return Err(format!("tako-server error: {}", response).into());
        }

        // Update current symlink only after tako-server accepted the deploy command.
        run_deploy_step(
            "Updating current symlink...",
            use_spinner,
            ssh.symlink(&release_dir, &config.current_link()),
        )
        .await?;

        // Clean up old releases (keep last 30 days).
        let releases_dir = format!("{}/releases", config.remote_base);
        let cleanup_cmd = format!(
            "find {} -mindepth 1 -maxdepth 1 -type d -mtime +30 -exec rm -rf {{}} \\;",
            releases_dir
        );
        run_deploy_step(
            "Cleaning old releases...",
            use_spinner,
            ssh.exec(&cleanup_cmd),
        )
        .await?;

        Ok(())
    }
    .await;

    if result.is_err() && !release_dir_preexisted {
        if let Err(e) = cleanup_partial_release(&ssh, &release_dir).await {
            tracing::warn!(
                release_dir = %release_dir,
                error = %e,
                "Failed to cleanup partial release directory"
            );
        }
    }

    // Always release lock (best-effort).
    let _ = ssh
        .exec(&format!("rmdir {} 2>/dev/null || true", lock_dir))
        .await;

    // Always disconnect (best-effort).
    let _ = ssh.disconnect().await;

    result
}

async fn check_tako_server(
    server: &ServerEntry,
) -> Result<TakoServerStatus, Box<dyn std::error::Error + Send + Sync>> {
    let ssh_config = SshConfig::from_server(&server.host, server.port);
    let mut ssh = SshClient::new(ssh_config);
    ssh.connect().await?;

    let installed = ssh.is_tako_installed().await?;
    if !installed {
        let _ = ssh.disconnect().await;
        return Ok(TakoServerStatus::Missing);
    }

    let running = is_tako_service_running(&ssh).await?;
    let _ = ssh.disconnect().await;
    Ok(if running {
        TakoServerStatus::Ready
    } else {
        TakoServerStatus::NotRunning
    })
}

async fn is_tako_service_running(
    ssh: &SshClient,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let status = ssh
        .tako_status()
        .await
        .unwrap_or_else(|_| String::from("unknown"));
    if let Some(running) = interpret_tako_service_status(&status) {
        return Ok(running);
    }

    // Non-systemd hosts or restrictive sudo policies can report "unknown".
    // Fall back to checking for a live process.
    let out = ssh
        .exec("pgrep -x tako-server >/dev/null 2>&1 && echo yes || echo no")
        .await?;
    Ok(out.stdout.trim() == "yes")
}

fn interpret_tako_service_status(status: &str) -> Option<bool> {
    let normalized = status.trim();
    if normalized == "active" {
        return Some(true);
    }
    if normalized == "unknown" {
        return None;
    }
    Some(false)
}

async fn ensure_tako_running(
    ssh: &mut SshClient,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    if is_tako_service_running(ssh).await? {
        return Ok(());
    }

    Err("tako-server is installed but not running. Start the service (e.g. `sudo systemctl start tako-server`), then retry.".into())
}

/// Format file size in human-readable format
fn format_size(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.2} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.2} KB", bytes as f64 / KB as f64)
    } else {
        format!("{} bytes", bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{EnvConfig, ServerConfig, ServerEntry, ServersToml, TakoToml};
    use std::collections::HashMap;
    use tempfile::TempDir;

    #[test]
    fn resolve_deploy_environment_prefers_explicit_env() {
        let mut config = TakoToml::default();
        config.envs.insert(
            "production".to_string(),
            EnvConfig {
                route: Some("prod.example.com".to_string()),
                ..Default::default()
            },
        );
        config.envs.insert(
            "staging".to_string(),
            EnvConfig {
                route: Some("staging.example.com".to_string()),
                ..Default::default()
            },
        );

        let resolved = resolve_deploy_environment(Some("staging"), &config).unwrap();
        assert_eq!(resolved, "staging");
    }

    #[test]
    fn resolve_deploy_environment_rejects_development() {
        let config = TakoToml::default();

        let err = resolve_deploy_environment(Some("development"), &config).unwrap_err();
        assert!(err.contains("reserved for local development"));
    }

    #[test]
    fn interpret_tako_service_status_handles_known_values() {
        assert_eq!(interpret_tako_service_status("active"), Some(true));
        assert_eq!(interpret_tako_service_status("unknown"), None);
        assert_eq!(interpret_tako_service_status("inactive"), Some(false));
        assert_eq!(interpret_tako_service_status("failed"), Some(false));
    }

    #[test]
    fn resolve_deploy_environment_defaults_to_production_with_single_server() {
        let mut config = TakoToml::default();
        config.envs.insert(
            "production".to_string(),
            EnvConfig {
                route: Some("prod.example.com".to_string()),
                ..Default::default()
            },
        );

        let resolved = resolve_deploy_environment(None, &config).unwrap();
        assert_eq!(resolved, "production");
    }

    #[test]
    fn resolve_deploy_environment_defaults_to_production() {
        let mut config = TakoToml::default();
        config.envs.insert(
            "production".to_string(),
            EnvConfig {
                route: Some("prod.example.com".to_string()),
                ..Default::default()
            },
        );

        let resolved = resolve_deploy_environment(None, &config).unwrap();
        assert_eq!(resolved, "production");
    }

    #[test]
    fn resolve_deploy_environment_rejects_missing_requested_environment() {
        let mut config = TakoToml::default();
        config.envs.insert(
            "production".to_string(),
            EnvConfig {
                route: Some("prod.example.com".to_string()),
                ..Default::default()
            },
        );

        let err = resolve_deploy_environment(Some("staging"), &config).unwrap_err();
        assert!(err.contains("Environment 'staging' not found"));
    }

    #[test]
    fn resolve_deploy_environment_rejects_missing_default_production_environment() {
        let mut config = TakoToml::default();
        config.envs.insert(
            "staging".to_string(),
            EnvConfig {
                route: Some("staging.example.com".to_string()),
                ..Default::default()
            },
        );

        let err = resolve_deploy_environment(None, &config).unwrap_err();
        assert!(err.contains("Environment 'production' not found"));
    }

    #[test]
    fn should_confirm_production_deploy_requires_interactive_unless_yes_is_set() {
        assert!(should_confirm_production_deploy("production", false, true));
        assert!(!should_confirm_production_deploy("production", true, true));
        assert!(!should_confirm_production_deploy(
            "production",
            false,
            false
        ));
        assert!(!should_confirm_production_deploy("staging", false, true));
    }

    #[test]
    fn format_production_deploy_confirm_prompt_is_short() {
        let prompt = format_production_deploy_confirm_prompt();
        assert!(prompt.contains("production"));
        assert!(!prompt.contains("--yes"));
    }

    #[test]
    fn format_production_deploy_confirm_hint_mentions_yes_flag() {
        let hint = format_production_deploy_confirm_hint();
        assert!(hint.contains("--yes"));
        assert!(hint.contains("-y"));
    }

    #[test]
    fn resolve_deploy_servers_prefers_explicit_mapping() {
        let mut config = TakoToml::default();
        config.servers.insert(
            "prod-1".to_string(),
            ServerConfig {
                env: "production".to_string(),
                instances: None,
                port: None,
                idle_timeout: None,
            },
        );

        let mut servers = ServersToml::default();
        servers.servers.insert(
            "prod-1".to_string(),
            ServerEntry {
                host: "127.0.0.1".to_string(),
                port: 22,
                description: None,
            },
        );

        let resolved =
            resolve_deploy_server_names(&config, &servers, "production", Some(false)).unwrap();
        assert_eq!(resolved, vec!["prod-1".to_string()]);
    }

    #[test]
    fn resolve_deploy_servers_can_fallback_to_single_global_production_server() {
        let config = TakoToml::default();

        let mut servers = ServersToml::default();
        servers.servers.insert(
            "solo".to_string(),
            ServerEntry {
                host: "127.0.0.1".to_string(),
                port: 22,
                description: None,
            },
        );

        let resolved =
            resolve_deploy_server_names(&config, &servers, "production", Some(true)).unwrap();
        assert_eq!(resolved, vec!["solo".to_string()]);
    }

    #[test]
    fn resolve_deploy_servers_can_cancel_single_global_production_fallback() {
        let config = TakoToml::default();

        let mut servers = ServersToml::default();
        servers.servers.insert(
            "solo".to_string(),
            ServerEntry {
                host: "127.0.0.1".to_string(),
                port: 22,
                description: None,
            },
        );

        let err =
            resolve_deploy_server_names(&config, &servers, "production", Some(false)).unwrap_err();
        assert!(err.contains("cancelled"));
    }

    #[test]
    fn resolve_deploy_servers_errors_with_hint_when_no_global_servers_exist() {
        let config = TakoToml::default();
        let servers = ServersToml::default();

        let err =
            resolve_deploy_server_names(&config, &servers, "production", Some(true)).unwrap_err();
        assert!(err.contains("No servers have been added"));
        assert!(err.contains("tako servers add <host>"));
    }

    #[test]
    fn resolve_deploy_servers_errors_for_non_production_without_mapping() {
        let config = TakoToml::default();
        let mut servers = ServersToml::default();
        servers.servers.insert(
            "solo".to_string(),
            ServerEntry {
                host: "127.0.0.1".to_string(),
                port: 22,
                description: None,
            },
        );

        let err =
            resolve_deploy_server_names(&config, &servers, "staging", Some(true)).unwrap_err();
        assert!(err.contains("No servers configured for environment 'staging'"));
    }

    #[test]
    fn format_production_mapping_example_uses_tako_toml_server_section() {
        let example = format_production_mapping_example("tako-server");
        assert!(example.contains("[servers.tako-server]"));
        assert!(example.contains("env = \"production\""));
    }

    #[test]
    fn format_single_server_production_confirm_prompt_mentions_persisting_mapping() {
        let prompt = format_single_server_production_confirm_prompt("tako-server");
        assert!(prompt.contains("tako-server"));
        assert!(prompt.contains("save this mapping"));
        assert!(prompt.contains("tako.toml"));
    }

    #[test]
    fn format_declined_single_server_reason_mentions_next_step() {
        let reason = format_declined_single_server_reason("solo");
        assert!(reason.contains("solo"));
        assert!(reason.contains("Add a different server now"));
        assert!(reason.contains("production"));
    }

    #[tokio::test]
    async fn choose_or_add_production_server_after_single_fallback_keeps_existing_when_confirmed() {
        let mut servers = ServersToml::default();
        servers.servers.insert(
            "solo".to_string(),
            ServerEntry {
                host: "127.0.0.1".to_string(),
                port: 22,
                description: None,
            },
        );

        let selected =
            choose_or_add_production_server_after_single_fallback(&mut servers, "solo", Some(true))
                .await
                .unwrap();
        assert_eq!(selected, "solo");
    }

    #[tokio::test]
    async fn choose_or_add_production_server_after_single_fallback_can_cancel_when_declined() {
        let mut servers = ServersToml::default();
        servers.servers.insert(
            "solo".to_string(),
            ServerEntry {
                host: "127.0.0.1".to_string(),
                port: 22,
                description: None,
            },
        );

        let err = choose_or_add_production_server_after_single_fallback(
            &mut servers,
            "solo",
            Some(false),
        )
        .await
        .unwrap_err();
        assert!(err.contains("Deployment cancelled"));
    }

    #[tokio::test]
    async fn resolve_deploy_servers_with_setup_persists_single_server_mapping() {
        let config = TakoToml::default();
        let mut servers = ServersToml::default();
        servers.servers.insert(
            "solo".to_string(),
            ServerEntry {
                host: "127.0.0.1".to_string(),
                port: 22,
                description: None,
            },
        );

        let temp_dir = TempDir::new().unwrap();

        let resolved = resolve_deploy_server_names_with_setup(
            &config,
            &mut servers,
            "production",
            temp_dir.path(),
        )
        .await
        .unwrap();
        assert_eq!(resolved, vec!["solo".to_string()]);

        let saved = TakoToml::load_from_dir(temp_dir.path()).unwrap();
        assert_eq!(saved.get_servers_for_env("production"), vec!["solo"]);
    }

    #[tokio::test]
    async fn resolve_deploy_servers_with_setup_requires_interactive_selection_when_multiple_servers()
     {
        let config = TakoToml::default();
        let mut servers = ServersToml::default();
        servers.servers.insert(
            "a".to_string(),
            ServerEntry {
                host: "10.0.0.1".to_string(),
                port: 22,
                description: None,
            },
        );
        servers.servers.insert(
            "b".to_string(),
            ServerEntry {
                host: "10.0.0.2".to_string(),
                port: 22,
                description: Some("backup".to_string()),
            },
        );

        let temp_dir = TempDir::new().unwrap();
        let err = resolve_deploy_server_names_with_setup(
            &config,
            &mut servers,
            "production",
            temp_dir.path(),
        )
        .await
        .unwrap_err();
        assert!(err.contains("No servers configured for environment 'production'"));
    }

    #[test]
    fn deploy_config_paths_are_derived_from_remote_base() {
        let cfg = DeployConfig {
            app_name: "my-app".to_string(),
            version: "v1".to_string(),
            archive_path: PathBuf::from("/tmp/archive.tar.gz"),
            remote_base: "/opt/tako/apps/my-app".to_string(),
            routes: vec![],
        };
        assert_eq!(cfg.release_dir(), "/opt/tako/apps/my-app/releases/v1");
        assert_eq!(cfg.current_link(), "/opt/tako/apps/my-app/current");
        assert_eq!(cfg.shared_dir(), "/opt/tako/apps/my-app/shared");
    }

    #[test]
    fn format_runtime_summary_omits_empty_version() {
        assert_eq!(format_runtime_summary("bun", None), "Runtime: bun");
        assert_eq!(format_runtime_summary("bun", Some("")), "Runtime: bun");
    }

    #[test]
    fn format_runtime_summary_includes_version_when_present() {
        assert_eq!(
            format_runtime_summary("bun", Some("1.3.9")),
            "Runtime: bun (1.3.9)"
        );
    }

    #[test]
    fn format_servers_summary_joins_server_names() {
        let names = vec!["a".to_string(), "b".to_string()];
        assert_eq!(format_servers_summary(&names), "Servers: a, b");
    }

    #[test]
    fn resolve_deploy_server_targets_requires_metadata_for_each_server() {
        let mut servers = ServersToml::default();
        servers.servers.insert(
            "prod-1".to_string(),
            ServerEntry {
                host: "10.0.0.1".to_string(),
                port: 22,
                description: None,
            },
        );

        let err = resolve_deploy_server_targets(&servers, &["prod-1".to_string()]).unwrap_err();
        assert!(err.contains("missing targets"));
        assert!(err.contains("prod-1"));
        assert!(err.contains("does not probe"));
    }

    #[test]
    fn resolve_deploy_server_targets_rejects_invalid_values() {
        let mut servers = ServersToml::default();
        servers.servers.insert(
            "prod-1".to_string(),
            ServerEntry {
                host: "10.0.0.1".to_string(),
                port: 22,
                description: None,
            },
        );
        servers.server_targets.insert(
            "prod-1".to_string(),
            ServerTarget {
                arch: "sparc".to_string(),
                libc: "glibc".to_string(),
            },
        );

        let err = resolve_deploy_server_targets(&servers, &["prod-1".to_string()]).unwrap_err();
        assert!(err.contains("invalid targets"));
        assert!(err.contains("sparc"));
    }

    #[test]
    fn format_server_targets_summary_deduplicates_target_labels() {
        let summary = format_server_targets_summary(&[
            (
                "a".to_string(),
                ServerTarget {
                    arch: "x86_64".to_string(),
                    libc: "glibc".to_string(),
                },
            ),
            (
                "b".to_string(),
                ServerTarget {
                    arch: "x86_64".to_string(),
                    libc: "glibc".to_string(),
                },
            ),
            (
                "c".to_string(),
                ServerTarget {
                    arch: "aarch64".to_string(),
                    libc: "musl".to_string(),
                },
            ),
        ]);

        assert_eq!(
            summary,
            "Server targets: linux-aarch64-musl, linux-x86_64-glibc"
        );
    }

    #[test]
    fn deploy_progress_helpers_render_preparing_and_single_line_server_results() {
        let section = format_prepare_deploy_section("production");
        assert!(section.contains("Preparing deployment for"));
        assert!(section.contains("production"));

        let server = ServerEntry {
            host: "example.com".to_string(),
            port: 2222,
            description: None,
        };
        assert_eq!(
            format_server_deploy_success("prod", &server),
            "prod (tako@example.com:2222) deployed successfully"
        );
        assert_eq!(
            format_server_deploy_failure("prod", &server, "boom"),
            "prod (tako@example.com:2222) deploy failed: boom"
        );
    }

    #[test]
    fn should_use_per_server_spinners_only_for_single_interactive_target() {
        assert!(should_use_per_server_spinners(1, true));
        assert!(!should_use_per_server_spinners(2, true));
        assert!(!should_use_per_server_spinners(1, false));
    }

    #[test]
    fn format_size_uses_expected_units() {
        assert_eq!(format_size(999), "999 bytes");
        assert_eq!(format_size(1024), "1.00 KB");
        assert_eq!(format_size(1024 * 1024), "1.00 MB");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.00 GB");
    }

    #[test]
    fn build_env_file_contents_includes_build_and_escaped_secrets() {
        let mut secrets = HashMap::new();
        secrets.insert("API_KEY".to_string(), r#"ab\"cd"#.to_string());
        secrets.insert("PATH_HINT".to_string(), r#"C:\tmp\bin"#.to_string());

        let env = build_env_file_contents("v123", Some(&secrets));
        assert!(env.contains("TAKO_BUILD=\"v123\""));
        assert!(env.contains(r#"API_KEY="ab\\\"cd""#));
        assert!(env.contains(r#"PATH_HINT="C:\\tmp\\bin""#));
    }

    #[test]
    fn build_env_file_contents_works_without_secrets() {
        let env = build_env_file_contents("v123", None);
        assert_eq!(env, "TAKO_BUILD=\"v123\"\n");
    }

    #[test]
    fn parse_existing_routes_from_ok_response_keeps_empty_routes_and_ignores_malformed_entries() {
        let response = Response::Ok {
            data: serde_json::json!({
                "routes": [
                    {"app": "good-a", "routes": ["a.example.com", "*.a.example.com"]},
                    {"app": "missing-routes"},
                    {"routes": ["missing-app.example.com"]},
                    {"app": "good-b", "routes": ["b.example.com/path/*"]}
                ]
            }),
        };

        let parsed = parse_existing_routes_response(response).expect("should parse");
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed[0].0, "good-a");
        assert_eq!(parsed[1].0, "missing-routes");
        assert!(parsed[1].1.is_empty());
        assert_eq!(parsed[2].0, "good-b");
    }

    #[test]
    fn parse_existing_routes_from_error_response_returns_message() {
        let response = Response::Error {
            message: "boom".to_string(),
        };
        let err = parse_existing_routes_response(response).unwrap_err();
        assert!(err.contains("boom"));
    }

    #[test]
    fn deploy_response_error_detection_handles_json_and_legacy_string_matches() {
        let json_err = r#"{"status":"error","message":"nope"}"#;
        let json_ok = r#"{"status":"ok","data":{}}"#;
        let legacy_err = r#"{"error":"legacy"}"#;
        let plain_text = "all good";

        assert!(deploy_response_has_error(json_err));
        assert!(!deploy_response_has_error(json_ok));
        assert!(deploy_response_has_error(legacy_err));
        assert!(!deploy_response_has_error(plain_text));
    }

    #[test]
    fn format_environment_not_found_error_handles_empty_and_non_empty_env_list() {
        let no_envs = format_environment_not_found_error("production", &[]);
        assert!(no_envs.contains("Environment 'production' not found"));
        assert!(no_envs.contains("(none)"));

        let with_envs = format_environment_not_found_error(
            "staging",
            &["production".to_string(), "dev".to_string()],
        );
        assert!(with_envs.contains("production, dev"));
    }

    #[test]
    fn deploy_error_message_helpers_include_expected_text() {
        let no_servers = format_no_servers_for_env_error("production");
        assert!(no_servers.contains("No servers configured for environment 'production'"));

        let no_global = format_no_global_servers_error();
        assert!(no_global.contains("No servers have been added"));
        assert!(no_global.contains("tako servers add <host>"));

        let missing_server = format_server_not_found_error("prod");
        assert!(missing_server.contains("Server 'prod' not found"));

        let not_running = format_tako_not_running_error(&["a".to_string(), "b".to_string()]);
        assert!(not_running.contains("a, b"));
        assert!(not_running.contains("not running"));

        let missing = format_tako_missing_error(&["x".to_string()]);
        assert!(missing.contains("not installed"));
        assert!(missing.contains("x"));

        let partial = format_partial_failure_error(2);
        assert_eq!(partial, "2 server(s) failed");
    }

    #[test]
    fn required_remote_free_bytes_adds_unpack_multiplier_and_headroom() {
        let archive_size = 10 * 1024 * 1024;
        let required = required_remote_free_bytes(archive_size);
        assert_eq!(
            required,
            archive_size.saturating_mul(3) + DEPLOY_DISK_SPACE_HEADROOM_BYTES
        );
        assert!(required > archive_size);
    }

    #[test]
    fn parse_df_available_kb_accepts_numeric_output() {
        assert_eq!(parse_df_available_kb("12345\n").unwrap(), 12345);
        assert_eq!(parse_df_available_kb("  98765  ").unwrap(), 98765);
    }

    #[test]
    fn parse_df_available_kb_rejects_empty_or_non_numeric_output() {
        assert!(parse_df_available_kb("").is_err());
        assert!(parse_df_available_kb("N/A").is_err());
        assert!(parse_df_available_kb("12.5").is_err());
    }

    #[test]
    fn format_insufficient_disk_space_error_includes_required_available_and_archive() {
        let msg = format_insufficient_disk_space_error(
            15 * 1024 * 1024,
            8 * 1024 * 1024,
            3 * 1024 * 1024,
        );
        assert!(msg.contains("Insufficient disk space under /opt/tako"));
        assert!(msg.contains("Archive size"));
        assert!(msg.contains("Required free space"));
        assert!(msg.contains("Available free space"));
    }

    #[test]
    fn cleanup_partial_release_command_uses_safe_single_quoted_path() {
        let cmd = cleanup_partial_release_command("/opt/tako/apps/a'b/releases/v1");
        assert!(cmd.contains("rm -rf"));
        assert!(cmd.contains("'\\''"));
        assert!(cmd.contains("/opt/tako/apps/"));
    }

    #[test]
    fn deploy_artifact_input_dir_is_under_dot_tako_artifacts_app() {
        let root = Path::new("/tmp/project");
        assert_eq!(
            deploy_artifact_input_dir(root),
            PathBuf::from("/tmp/project/.tako/artifacts/app")
        );
    }

    #[test]
    fn ensure_artifact_input_ready_requires_existing_directory() {
        let temp = TempDir::new().unwrap();
        let missing = temp.path().join(".tako/artifacts/app");
        let err = ensure_artifact_input_ready(&missing).unwrap_err();
        assert!(err.contains(".tako/artifacts/app"));
        assert!(err.contains("must contain build artifacts"));
    }

    #[test]
    fn ensure_artifact_input_ready_rejects_empty_directory() {
        let temp = TempDir::new().unwrap();
        let artifact_dir = temp.path().join(".tako/artifacts/app");
        std::fs::create_dir_all(&artifact_dir).unwrap();
        let err = ensure_artifact_input_ready(&artifact_dir).unwrap_err();
        assert!(err.contains("is empty"));
    }

    #[test]
    fn ensure_artifact_input_ready_accepts_nested_files() {
        let temp = TempDir::new().unwrap();
        let artifact_dir = temp.path().join(".tako/artifacts/app");
        std::fs::create_dir_all(artifact_dir.join("sub")).unwrap();
        std::fs::write(artifact_dir.join("sub/index.js"), "ok").unwrap();
        ensure_artifact_input_ready(&artifact_dir).expect("build artifacts should be accepted");
    }

    #[test]
    fn merge_configured_asset_roots_merges_into_static_without_overwriting_existing_files() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().join("project");
        let artifact_dir = project_dir.join(".tako/artifacts/app");
        let static_dir = artifact_dir.join("static");

        std::fs::create_dir_all(project_dir.join("assets/icons")).unwrap();
        std::fs::create_dir_all(&static_dir).unwrap();

        std::fs::write(project_dir.join("assets/logo.svg"), "from-assets").unwrap();
        std::fs::write(project_dir.join("assets/icons/app.svg"), "nested-asset").unwrap();
        std::fs::write(static_dir.join("logo.svg"), "existing-client").unwrap();

        merge_configured_asset_roots(&project_dir, &artifact_dir, &["assets".to_string()]).unwrap();

        assert_eq!(
            std::fs::read_to_string(static_dir.join("logo.svg")).unwrap(),
            "existing-client"
        );
        assert_eq!(
            std::fs::read_to_string(static_dir.join("icons/app.svg")).unwrap(),
            "nested-asset"
        );
    }

    #[test]
    fn merge_configured_asset_roots_falls_back_to_public_dir_when_static_missing() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().join("project");
        let artifact_dir = project_dir.join(".tako/artifacts/app");
        let public_dir = artifact_dir.join("public");

        std::fs::create_dir_all(project_dir.join("shared-assets")).unwrap();
        std::fs::create_dir_all(&public_dir).unwrap();
        std::fs::write(
            project_dir.join("shared-assets/robots.txt"),
            "User-agent: *",
        )
        .unwrap();

        merge_configured_asset_roots(&project_dir, &artifact_dir, &["shared-assets".to_string()])
            .unwrap();

        assert_eq!(
            std::fs::read_to_string(public_dir.join("robots.txt")).unwrap(),
            "User-agent: *"
        );
    }

    #[test]
    fn merge_configured_asset_roots_errors_when_source_directory_is_missing() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().join("project");
        let artifact_dir = project_dir.join(".tako/artifacts/app");
        std::fs::create_dir_all(artifact_dir.join("static")).unwrap();

        let err = merge_configured_asset_roots(
            &project_dir,
            &artifact_dir,
            &["missing-assets".to_string()],
        )
        .unwrap_err();
        assert!(err.contains("does not exist"));
        assert!(err.contains("missing-assets"));
    }

    #[test]
    fn build_deploy_archive_manifest_includes_sorted_env_and_secret_names() {
        let app_env_vars = HashMap::from([
            ("Z_KEY".to_string(), "z".to_string()),
            ("A_KEY".to_string(), "a".to_string()),
        ]);
        let runtime_env_vars = HashMap::from([
            ("NODE_ENV".to_string(), "production".to_string()),
            ("BUN_ENV".to_string(), "production".to_string()),
        ]);
        let secrets = HashMap::from([
            ("API_KEY".to_string(), "x".to_string()),
            ("DB_URL".to_string(), "y".to_string()),
        ]);

        let manifest = build_deploy_archive_manifest(
            "my-app",
            "staging",
            "v1",
            "bun",
            Path::new("index.ts"),
            app_env_vars,
            runtime_env_vars,
            Some(&secrets),
        );

        assert_eq!(manifest.app_name, "my-app");
        assert_eq!(manifest.environment, "staging");
        assert_eq!(manifest.version, "v1");
        assert_eq!(manifest.runtime, "bun");
        assert_eq!(
            manifest.env_vars.keys().cloned().collect::<Vec<_>>(),
            vec![
                "A_KEY".to_string(),
                "BUN_ENV".to_string(),
                "NODE_ENV".to_string(),
                "TAKO_ENV".to_string(),
                "Z_KEY".to_string()
            ]
        );
        assert_eq!(
            manifest.env_vars.get("TAKO_ENV"),
            Some(&"staging".to_string())
        );
        assert_eq!(
            manifest.env_vars.get("NODE_ENV"),
            Some(&"staging".to_string())
        );
        assert_eq!(
            manifest.env_vars.get("BUN_ENV"),
            Some(&"staging".to_string())
        );
        assert_eq!(
            manifest.secret_names,
            vec!["API_KEY".to_string(), "DB_URL".to_string()]
        );
    }

    #[test]
    fn stage_archive_package_writes_manifest_and_artifacts_tree() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source");
        let package = temp.path().join("package");
        std::fs::create_dir_all(source.join("nested")).unwrap();
        std::fs::write(source.join("nested/app.js"), "console.log('ok')").unwrap();

        let manifest = DeployArchiveManifest {
            app_name: "my-app".to_string(),
            environment: "production".to_string(),
            version: "v1".to_string(),
            runtime: "bun".to_string(),
            entry_point: "index.ts".to_string(),
            env_vars: BTreeMap::from([("NODE_ENV".to_string(), "production".to_string())]),
            secret_names: vec!["API_KEY".to_string()],
        };

        prepare_archive_package_dir(&package).unwrap();
        stage_archive_package(&source, &package, &manifest).unwrap();

        assert!(package.join("artifacts/nested/app.js").exists());
        let manifest_path = package.join(DEPLOY_ARCHIVE_MANIFEST_FILE);
        assert!(manifest_path.exists());

        let decoded: DeployArchiveManifest =
            serde_json::from_str(&std::fs::read_to_string(manifest_path).unwrap()).unwrap();
        assert_eq!(decoded, manifest);
    }
}
