use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::env::current_dir;
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::app::resolve_app_name;
use crate::build::{
    BuildAdapter, BuildCache, BuildError, BuildExecutor, BuildPreset, BuildStageCommand,
    ResolvedPresetSource, apply_adapter_base_runtime_defaults, compute_file_hash,
    create_filtered_archive_with_prefix, infer_adapter_from_preset_reference, load_build_preset,
    qualify_runtime_local_preset_ref, run_container_build,
};
use crate::commands::server;
use crate::config::{BuildStage, SecretsStore, ServerEntry, ServerTarget, ServersToml, TakoToml};
use crate::output;
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
    remote_base: String,
    routes: Vec<String>,
    env_vars: HashMap<String, String>,
    app_subdir: String,
    runtime: String,
    main: String,
    deploy_install: Option<String>,
    deploy_start: Vec<String>,
}

#[derive(Clone)]
struct ServerDeployTarget {
    name: String,
    server: ServerEntry,
    target_label: String,
    archive_path: PathBuf,
    instances: u8,
    idle_timeout: u32,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct DeployArchiveManifest {
    app_name: String,
    environment: String,
    version: String,
    runtime: String,
    main: String,
    env_vars: BTreeMap<String, String>,
    secret_names: Vec<String>,
}

struct ValidationResult {
    tako_config: TakoToml,
    servers: ServersToml,
    secrets: SecretsStore,
    env: String,
    warnings: Vec<String>,
}

const ARTIFACT_CACHE_SCHEMA_VERSION: u32 = 3;
const ARTIFACT_CACHE_LOCK_TIMEOUT_SECS: u64 = 10 * 60;
const ARTIFACT_CACHE_STALE_LOCK_SECS: u64 = 30 * 60;
const LOCAL_ARTIFACT_CACHE_KEEP_SOURCE_ARCHIVES: usize = 30;
const LOCAL_ARTIFACT_CACHE_KEEP_TARGET_ARTIFACTS: usize = 90;
const LOCAL_BUILD_WORKSPACE_RELATIVE_DIR: &str = ".tako/build-workspaces";
const RUNTIME_VERSION_OUTPUT_FILE: &str = ".tako-runtime-version";
const PROTO_TOOLS_FILE: &str = ".prototools";

#[derive(serde::Serialize)]
struct ArtifactCacheKeyInput<'a> {
    schema_version: u32,
    source_hash: &'a str,
    runtime: &'a str,
    runtime_tool: &'a str,
    runtime_version: &'a str,
    target_label: &'a str,
    preset_ref: &'a str,
    preset_repo: &'a str,
    preset_path: &'a str,
    preset_commit: &'a str,
    app_subdir: &'a str,
    builder_image: Option<&'a str>,
    use_docker: bool,
    install: Option<&'a str>,
    build: Option<&'a str>,
    custom_stages: &'a [BuildStage],
    include_patterns: &'a [String],
    exclude_patterns: &'a [String],
    asset_roots: &'a [String],
}

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct ArtifactCacheMetadata {
    schema_version: u32,
    cache_key: String,
    artifact_sha256: String,
    artifact_size: u64,
}

#[derive(Debug, Clone)]
struct ArtifactCachePaths {
    artifact_path: PathBuf,
    metadata_path: PathBuf,
    lock_path: PathBuf,
}

#[derive(Debug, Clone)]
struct CachedArtifact {
    path: PathBuf,
    size_bytes: u64,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct LocalArtifactCacheCleanupSummary {
    removed_source_archives: usize,
    removed_target_artifacts: usize,
    removed_target_metadata: usize,
}

impl LocalArtifactCacheCleanupSummary {
    fn total_removed(self) -> usize {
        self.removed_source_archives + self.removed_target_artifacts + self.removed_target_metadata
    }
}

#[derive(Debug)]
struct ArtifactCacheLockGuard {
    lock_path: PathBuf,
}

impl Drop for ArtifactCacheLockGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.lock_path);
    }
}

impl DeployConfig {
    fn release_dir(&self) -> String {
        format!("{}/releases/{}", self.remote_base, self.version)
    }

    fn release_app_dir(&self) -> String {
        if self.app_subdir.is_empty() {
            self.release_dir()
        } else {
            format!("{}/{}", self.release_dir(), self.app_subdir)
        }
    }

    fn release_app_manifest_path(&self) -> String {
        format!(
            "{}/{}",
            self.release_app_dir(),
            DEPLOY_ARCHIVE_MANIFEST_FILE
        )
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
                return Err(format!(
                    "Secret errors:\n  {}",
                    secrets_result.errors.join("\n  ")
                ));
            }
            warnings.extend(secrets_result.warnings.clone());

            Ok(ValidationResult {
                tako_config,
                servers,
                secrets,
                env,
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
        ..
    } = validation;

    confirm_production_deploy(&env, assume_yes)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    output::section(&format_prepare_deploy_section(&env));

    let app_name = resolve_app_name(&project_dir).map_err(|e| -> Box<dyn std::error::Error> {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()).into()
    })?;
    let routes = required_env_routes(&tako_config, &env)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    output::success("Configuration valid");

    // ===== Build =====
    output::section("Build");

    let executor = BuildExecutor::new(&project_dir);
    let cache = BuildCache::new(project_dir.join(".tako/artifacts"));
    cache
        .init()
        .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
    match cleanup_local_artifact_cache(
        cache.cache_dir(),
        LOCAL_ARTIFACT_CACHE_KEEP_SOURCE_ARCHIVES,
        LOCAL_ARTIFACT_CACHE_KEEP_TARGET_ARTIFACTS,
    ) {
        Ok(summary) if summary.total_removed() > 0 => output::muted(&format!(
            "Local artifact cache cleanup: removed {} old source archive(s), {} old artifact(s), {} stale metadata file(s)",
            summary.removed_source_archives,
            summary.removed_target_artifacts,
            summary.removed_target_metadata
        )),
        Ok(_) => {}
        Err(error) => output::warning(&format!("Local artifact cache cleanup skipped: {}", error)),
    }
    let build_workspace_root = project_dir.join(LOCAL_BUILD_WORKSPACE_RELATIVE_DIR);
    match cleanup_local_build_workspaces(&build_workspace_root) {
        Ok(removed) if removed > 0 => output::muted(&format!(
            "Local build workspace cleanup: removed {} workspace(s)",
            removed
        )),
        Ok(_) => {}
        Err(error) => output::warning(&format!("Local build workspace cleanup skipped: {}", error)),
    }

    let source_root = source_bundle_root(&project_dir);
    let app_subdir = resolve_app_subdir(&source_root, &project_dir)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    output::step(&format!("Source root: {}", source_root.display()));
    if !app_subdir.is_empty() {
        output::step(&format!("App directory: {}", app_subdir));
    }

    // Generate version string
    let (version, source_hash) = resolve_deploy_version_and_source_hash(&executor, &source_root)?;
    output::step(&format!("Version: {}", version));

    let preset_ref = resolve_build_preset_ref(&project_dir, &tako_config)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let runtime_adapter = resolve_effective_build_adapter(&project_dir, &tako_config, &preset_ref)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let (mut build_preset, resolved_preset) = output::with_spinner_async(
        "Resolving build preset...",
        load_build_preset(&project_dir, &preset_ref),
    )
    .await
    .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?
    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    apply_adapter_base_runtime_defaults(&mut build_preset, runtime_adapter)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    output::success(&format!(
        "Build preset: {} @ {}",
        resolved_preset.preset_ref,
        shorten_commit(&resolved_preset.commit)
    ));
    output::success(&format_runtime_summary(&build_preset.name, None));
    let runtime_tool = resolve_proto_runtime_tool(&build_preset.name, &build_preset)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    let manifest_main = resolve_deploy_main(&tako_config, build_preset.main.as_deref())
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    output::success(&format_entry_point_summary(
        &project_dir.join(&manifest_main),
    ));

    let manifest = build_deploy_archive_manifest(
        &app_name,
        &env,
        &version,
        &build_preset.name,
        &manifest_main,
        tako_config.get_merged_vars(&env),
        HashMap::new(),
        secrets.get_env(&env),
    );
    let deploy_env_vars = build_deploy_command_env_vars(&manifest, secrets.get_env(&env));

    // Create source archive used as input for target-specific builds.
    let source_archive_path = cache.cache_dir().join(format!("{}-source.tar.gz", version));
    let app_json_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
    let app_manifest_archive_path = archive_app_manifest_path(&app_subdir);
    let source_archive_size = output::with_spinner("Creating source archive...", || {
        executor.create_source_archive_with_extra_files(
            &source_root,
            &source_archive_path,
            &[(
                app_manifest_archive_path.as_str(),
                app_json_bytes.as_slice(),
            )],
        )
    })?
    .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;

    output::success(&format!(
        "Source archive created: {} ({})",
        format_path_relative_to(&project_dir, &source_archive_path),
        format_size(source_archive_size)
    ));

    let include_patterns = build_artifact_include_patterns(&tako_config);
    let exclude_patterns = build_artifact_exclude_patterns(&build_preset, &tako_config);
    let asset_roots = build_asset_roots(&build_preset, &tako_config)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    if !exclude_patterns.is_empty() {
        output::muted(&format!(
            "Artifact exclude patterns: {}",
            exclude_patterns.join(", ")
        ));
    }

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
    output::success(&format_servers_summary(&server_names));
    output::success(&format_server_targets_summary(&server_targets));

    let artifacts_by_target = build_target_artifacts(
        &project_dir,
        cache.cache_dir(),
        &build_workspace_root,
        &source_archive_path,
        &source_hash,
        &version,
        &app_subdir,
        &build_preset.name,
        &runtime_tool,
        &manifest_main,
        &server_targets,
        &build_preset,
        &resolved_preset,
        &tako_config.build.stages,
        &include_patterns,
        &exclude_patterns,
        &asset_roots,
    )
    .await
    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    // ===== Deploy =====
    output::section("Deploy");

    let deploy_config = Arc::new(DeployConfig {
        app_name: app_name.clone(),
        version: version.clone(),
        remote_base: format!("/opt/tako/apps/{}", app_name),
        routes,
        env_vars: deploy_env_vars,
        app_subdir,
        runtime: build_preset.name.clone(),
        main: manifest_main,
        deploy_install: build_preset.install.clone(),
        deploy_start: build_preset.start.clone(),
    });
    let target_by_server: HashMap<String, ServerTarget> = server_targets.into_iter().collect();

    // Build per-server deploy targets (includes per-server scaling settings)
    let mut targets = Vec::new();
    for server_name in &server_names {
        let server = servers.get(server_name).unwrap().clone();
        let target = target_by_server.get(server_name).ok_or_else(|| {
            format!(
                "Missing resolved target metadata for server '{}'",
                server_name
            )
        })?;
        let target_label = target.label();
        let archive_path = artifacts_by_target.get(&target_label).ok_or_else(|| {
            format!(
                "Missing build artifact for server target '{}'; expected artifact for {}",
                target_label, server_name
            )
        })?;
        targets.push(ServerDeployTarget {
            name: server_name.clone(),
            server,
            target_label,
            archive_path: archive_path.clone(),
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
        let target_label = target.target_label.clone();
        let archive_path = target.archive_path.clone();
        let deploy_config = deploy_config.clone();
        let instances = target.instances;
        let idle_timeout = target.idle_timeout;
        let use_spinner = use_per_server_spinners;
        let handle = tokio::spawn(async move {
            let result = deploy_to_server(
                &deploy_config,
                &server,
                &archive_path,
                &target_label,
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

fn format_build_completed_message(target_label: &str) -> String {
    format!("Build completed for {}", target_label)
}

fn format_prepare_artifact_message(target_label: &str) -> String {
    format!("Preparing artifact for {}...", target_label)
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

const DEPLOY_ARCHIVE_MANIFEST_FILE: &str = "app.json";

fn git_repo_root(project_dir: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(project_dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

fn source_bundle_root(project_dir: &Path) -> PathBuf {
    match git_repo_root(project_dir) {
        Some(root) if project_dir.starts_with(&root) => root,
        _ => project_dir.to_path_buf(),
    }
}

fn resolve_app_subdir(source_root: &Path, project_dir: &Path) -> Result<String, String> {
    let rel = project_dir.strip_prefix(source_root).map_err(|_| {
        format!(
            "Project directory {} must be within source root {}",
            project_dir.display(),
            source_root.display()
        )
    })?;
    if rel.as_os_str().is_empty() {
        return Ok(String::new());
    }
    Ok(rel.to_string_lossy().replace('\\', "/"))
}

fn resolve_deploy_version_and_source_hash(
    executor: &BuildExecutor,
    source_root: &Path,
) -> Result<(String, String), BuildError> {
    let source_hash = executor.compute_source_hash(source_root)?;
    let version = executor.generate_version(Some(&source_hash))?;
    Ok((version, source_hash))
}

fn archive_app_manifest_path(app_subdir: &str) -> String {
    if app_subdir.is_empty() {
        DEPLOY_ARCHIVE_MANIFEST_FILE.to_string()
    } else {
        format!("{}/{}", app_subdir, DEPLOY_ARCHIVE_MANIFEST_FILE)
    }
}

fn normalize_main_path(value: &str, source: &str) -> Result<String, String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("{source} main is empty"));
    }

    let raw_path = Path::new(trimmed);
    if raw_path.is_absolute() {
        return Err(format!(
            "{source} main '{trimmed}' must be relative to project root"
        ));
    }

    let mut normalized = trimmed.replace('\\', "/");
    while let Some(stripped) = normalized.strip_prefix("./") {
        normalized = stripped.to_string();
    }
    if normalized.starts_with('/') {
        return Err(format!(
            "{source} main '{trimmed}' must be relative to project root"
        ));
    }
    if Path::new(&normalized)
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(format!("{source} main '{trimmed}' must not contain '..'"));
    }
    if normalized.is_empty() {
        return Err(format!("{source} main is empty"));
    }
    Ok(normalized)
}

pub(crate) fn resolve_deploy_main(
    tako_config: &TakoToml,
    preset_main: Option<&str>,
) -> Result<String, String> {
    if let Some(main) = &tako_config.main {
        return normalize_main_path(main, "tako.toml");
    }

    if let Some(main) = preset_main {
        return normalize_main_path(main, "build preset");
    }

    Err("No deploy entrypoint configured. Set `main` in tako.toml or preset `main`.".to_string())
}

fn build_deploy_archive_manifest(
    app_name: &str,
    environment: &str,
    version: &str,
    runtime_name: &str,
    main: &str,
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
        main: main.to_string(),
        env_vars: build_manifest_env_vars(
            app_env_vars,
            runtime_env_vars,
            environment,
            runtime_name,
        ),
        secret_names,
    }
}

fn build_deploy_command_env_vars(
    manifest: &DeployArchiveManifest,
    env_secrets: Option<&HashMap<String, String>>,
) -> HashMap<String, String> {
    let mut env_vars: HashMap<String, String> = manifest
        .env_vars
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    env_vars.insert("TAKO_BUILD".to_string(), manifest.version.clone());
    if let Some(secrets) = env_secrets {
        for (key, value) in secrets {
            env_vars.insert(key.clone(), value.clone());
        }
    }
    env_vars
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

fn format_runtime_summary(runtime_name: &str, version: Option<&str>) -> String {
    match version.map(str::trim) {
        Some(version) if !version.is_empty() => {
            format!("Runtime: {} ({})", runtime_name, version)
        }
        _ => format!("Runtime: {}", runtime_name),
    }
}

fn resolve_proto_runtime_tool(runtime_name: &str, preset: &BuildPreset) -> Result<String, String> {
    if preset.start.len() >= 3
        && preset.start[0] == "proto"
        && preset.start[1] == "run"
        && !preset.start[2].trim().is_empty()
    {
        return Ok(preset.start[2].trim().to_string());
    }

    if runtime_name == "bun" || runtime_name.starts_with("bun-") {
        return Ok("bun".to_string());
    }

    Err(format!(
        "Unable to infer proto runtime tool for preset '{}'. Set preset `start` to begin with [\"proto\", \"run\", \"<tool>\", ...].",
        runtime_name
    ))
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

fn shorten_commit(commit: &str) -> &str {
    &commit[..commit.len().min(12)]
}

fn build_artifact_include_patterns(config: &TakoToml) -> Vec<String> {
    if !config.build.include.is_empty() {
        return config.build.include.clone();
    }
    vec!["**/*".to_string()]
}

#[cfg(test)]
fn should_report_artifact_include_patterns(include_patterns: &[String]) -> bool {
    if include_patterns.is_empty() {
        return false;
    }
    !(include_patterns.len() == 1 && include_patterns[0] == "**/*")
}

fn build_artifact_exclude_patterns(preset: &BuildPreset, config: &TakoToml) -> Vec<String> {
    let mut excludes = preset.build.exclude.clone();
    excludes.extend(config.build.exclude.clone());
    excludes
}

fn build_asset_roots(preset: &BuildPreset, config: &TakoToml) -> Result<Vec<String>, String> {
    let mut merged = Vec::new();
    for root in preset.assets.iter().chain(config.build.assets.iter()) {
        let normalized = normalize_asset_root(root)?;
        if !merged.contains(&normalized) {
            merged.push(normalized);
        }
    }
    Ok(merged)
}

fn resolve_build_target(
    preset: &BuildPreset,
    target_label: &str,
) -> Result<crate::build::BuildPresetTarget, String> {
    if !preset.build.targets.is_empty()
        && !preset
            .build
            .targets
            .iter()
            .any(|value| value == target_label)
    {
        let mut available = preset.build.targets.clone();
        available.sort();
        return Err(format!(
            "Build preset does not define target '{}'. Available targets: {}",
            target_label,
            if available.is_empty() {
                "(none)".to_string()
            } else {
                available.join(", ")
            }
        ));
    }

    Ok(crate::build::BuildPresetTarget {
        builder_image: None,
        install: preset.build.install.clone(),
        build: preset.build.build.clone(),
    })
}

fn should_use_docker_build(preset: &BuildPreset) -> bool {
    preset.build.container
}

fn resolve_build_adapter(
    project_dir: &Path,
    tako_config: &TakoToml,
) -> Result<BuildAdapter, String> {
    if let Some(adapter_override) = tako_config
        .runtime
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return BuildAdapter::from_id(adapter_override).ok_or_else(|| {
            format!(
                "Invalid runtime '{}'; expected one of: bun, node, deno",
                adapter_override
            )
        });
    }

    Ok(crate::build::detect_build_adapter(project_dir))
}

fn resolve_effective_build_adapter(
    project_dir: &Path,
    tako_config: &TakoToml,
    preset_ref: &str,
) -> Result<BuildAdapter, String> {
    let configured_or_detected = resolve_build_adapter(project_dir, tako_config)?;
    if configured_or_detected != BuildAdapter::Unknown {
        return Ok(configured_or_detected);
    }

    let inferred = infer_adapter_from_preset_reference(preset_ref);
    if inferred != BuildAdapter::Unknown {
        return Ok(inferred);
    }

    Ok(configured_or_detected)
}

fn resolve_build_preset_ref(project_dir: &Path, tako_config: &TakoToml) -> Result<String, String> {
    let runtime = resolve_build_adapter(project_dir, tako_config)?;
    if let Some(preset_ref) = tako_config
        .preset
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return qualify_runtime_local_preset_ref(runtime, preset_ref);
    }
    Ok(runtime.default_preset().to_string())
}

fn sanitize_cache_label(label: &str) -> String {
    label
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn artifact_cache_paths(
    cache_dir: &Path,
    target_label: &str,
    cache_key: &str,
) -> ArtifactCachePaths {
    let label = sanitize_cache_label(target_label);
    let stem = format!("artifact-cache-{}-{}", label, cache_key);
    ArtifactCachePaths {
        artifact_path: cache_dir.join(format!("{}.tar.gz", stem)),
        metadata_path: cache_dir.join(format!("{}.json", stem)),
        lock_path: cache_dir.join(format!("{}.lock", stem)),
    }
}

fn build_artifact_cache_key(
    source_hash: &str,
    runtime_name: &str,
    runtime_tool: &str,
    runtime_version: &str,
    use_docker: bool,
    target_label: &str,
    preset_source: &ResolvedPresetSource,
    target_build: &crate::build::BuildPresetTarget,
    custom_stages: &[BuildStage],
    include_patterns: &[String],
    exclude_patterns: &[String],
    asset_roots: &[String],
    app_subdir: &str,
) -> Result<String, String> {
    use sha2::{Digest, Sha256};

    let payload = ArtifactCacheKeyInput {
        schema_version: ARTIFACT_CACHE_SCHEMA_VERSION,
        source_hash,
        runtime: runtime_name,
        runtime_tool,
        runtime_version,
        target_label,
        preset_ref: &preset_source.preset_ref,
        preset_repo: &preset_source.repo,
        preset_path: &preset_source.path,
        preset_commit: &preset_source.commit,
        app_subdir,
        builder_image: target_build.builder_image.as_deref(),
        use_docker,
        install: target_build.install.as_deref(),
        build: target_build.build.as_deref(),
        custom_stages,
        include_patterns,
        exclude_patterns,
        asset_roots,
    };
    let bytes = serde_json::to_vec(&payload)
        .map_err(|e| format!("Failed to serialize build artifact cache key input: {e}"))?;

    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Ok(hex::encode(hasher.finalize()))
}

fn artifact_cache_temp_path(final_path: &Path) -> Result<PathBuf, String> {
    let file_name = final_path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("Invalid cache artifact filename '{}'", final_path.display()))?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp_name = format!("{}.tmp-{}-{}", file_name, std::process::id(), nanos);
    Ok(final_path.with_file_name(tmp_name))
}

fn cleanup_local_artifact_cache(
    cache_dir: &Path,
    keep_source_archives: usize,
    keep_target_artifacts: usize,
) -> Result<LocalArtifactCacheCleanupSummary, String> {
    if !cache_dir.exists() {
        return Ok(LocalArtifactCacheCleanupSummary::default());
    }
    let mut source_archives = Vec::new();
    let mut target_archives = Vec::new();
    let mut target_metadata = Vec::new();

    for entry in std::fs::read_dir(cache_dir)
        .map_err(|e| format!("Failed to read {}: {e}", cache_dir.display()))?
    {
        let entry = entry
            .map_err(|e| format!("Failed to read dir entry in {}: {e}", cache_dir.display()))?;
        let path = entry.path();
        let file_name = match path.file_name().and_then(|name| name.to_str()) {
            Some(name) => name,
            None => continue,
        };
        let metadata = entry
            .metadata()
            .map_err(|e| format!("Failed to read metadata for {}: {e}", path.display()))?;
        if !metadata.is_file() {
            continue;
        }

        if file_name.ends_with("-source.tar.gz") {
            source_archives.push((path, metadata.modified().unwrap_or(UNIX_EPOCH)));
            continue;
        }
        if file_name.starts_with("artifact-cache-") && file_name.ends_with(".tar.gz") {
            target_archives.push((path, metadata.modified().unwrap_or(UNIX_EPOCH)));
            continue;
        }
        if file_name.starts_with("artifact-cache-") && file_name.ends_with(".json") {
            target_metadata.push(path);
        }
    }

    source_archives.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| right.0.cmp(&left.0)));
    target_archives.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| right.0.cmp(&left.0)));

    let mut summary = LocalArtifactCacheCleanupSummary::default();

    for (path, _) in source_archives.into_iter().skip(keep_source_archives) {
        std::fs::remove_file(&path)
            .map_err(|e| format!("Failed to remove source archive {}: {e}", path.display()))?;
        summary.removed_source_archives += 1;
    }

    for (path, _) in target_archives.into_iter().skip(keep_target_artifacts) {
        std::fs::remove_file(&path)
            .map_err(|e| format!("Failed to remove artifact cache {}: {e}", path.display()))?;
        summary.removed_target_artifacts += 1;

        if let Some(metadata_path) = artifact_cache_metadata_path_for_archive(&path)
            && metadata_path.exists()
        {
            std::fs::remove_file(&metadata_path).map_err(|e| {
                format!(
                    "Failed to remove artifact metadata {}: {e}",
                    metadata_path.display()
                )
            })?;
            summary.removed_target_metadata += 1;
        }
    }

    for metadata_path in target_metadata {
        let Some(archive_path) = artifact_cache_archive_path_for_metadata(&metadata_path) else {
            continue;
        };
        if archive_path.exists() || !metadata_path.exists() {
            continue;
        }
        std::fs::remove_file(&metadata_path).map_err(|e| {
            format!(
                "Failed to remove orphan artifact metadata {}: {e}",
                metadata_path.display()
            )
        })?;
        summary.removed_target_metadata += 1;
    }

    Ok(summary)
}

fn cleanup_local_build_workspaces(workspace_root: &Path) -> Result<usize, String> {
    if !workspace_root.exists() {
        return Ok(0);
    }

    let mut removed = 0usize;
    for entry in std::fs::read_dir(workspace_root)
        .map_err(|e| format!("Failed to read {}: {e}", workspace_root.display()))?
    {
        let entry = entry.map_err(|e| {
            format!(
                "Failed to read dir entry in {}: {e}",
                workspace_root.display()
            )
        })?;
        let path = entry.path();
        if path.is_dir() {
            std::fs::remove_dir_all(&path)
                .map_err(|e| format!("Failed to remove build workspace {}: {e}", path.display()))?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn artifact_cache_metadata_path_for_archive(archive_path: &Path) -> Option<PathBuf> {
    let file_name = archive_path.file_name()?.to_str()?;
    let stem = file_name.strip_suffix(".tar.gz")?;
    Some(archive_path.with_file_name(format!("{stem}.json")))
}

fn artifact_cache_archive_path_for_metadata(metadata_path: &Path) -> Option<PathBuf> {
    let file_name = metadata_path.file_name()?.to_str()?;
    let stem = file_name.strip_suffix(".json")?;
    Some(metadata_path.with_file_name(format!("{stem}.tar.gz")))
}

fn remove_cached_artifact_files(paths: &ArtifactCachePaths) {
    let _ = std::fs::remove_file(&paths.artifact_path);
    let _ = std::fs::remove_file(&paths.metadata_path);
}

fn load_valid_cached_artifact(
    paths: &ArtifactCachePaths,
    expected_key: &str,
) -> Result<Option<CachedArtifact>, String> {
    if !paths.artifact_path.exists() || !paths.metadata_path.exists() {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(&paths.metadata_path).map_err(|e| {
        format!(
            "Failed to read cache metadata {}: {e}",
            paths.metadata_path.display()
        )
    })?;
    let metadata: ArtifactCacheMetadata = serde_json::from_str(&raw).map_err(|e| {
        format!(
            "Failed to parse cache metadata {}: {e}",
            paths.metadata_path.display()
        )
    })?;

    if metadata.schema_version != ARTIFACT_CACHE_SCHEMA_VERSION {
        return Err(format!(
            "cache schema mismatch (found {}, expected {})",
            metadata.schema_version, ARTIFACT_CACHE_SCHEMA_VERSION
        ));
    }
    if metadata.cache_key != expected_key {
        return Err("cache metadata key mismatch".to_string());
    }

    let artifact_size = std::fs::metadata(&paths.artifact_path)
        .map_err(|e| {
            format!(
                "Failed to stat cached artifact {}: {e}",
                paths.artifact_path.display()
            )
        })?
        .len();
    if artifact_size != metadata.artifact_size {
        return Err(format!(
            "cached artifact size mismatch (metadata {}, file {})",
            metadata.artifact_size, artifact_size
        ));
    }

    let actual_sha = compute_file_hash(&paths.artifact_path).map_err(|e| {
        format!(
            "Failed to hash cached artifact {}: {e}",
            paths.artifact_path.display()
        )
    })?;
    if actual_sha != metadata.artifact_sha256 {
        return Err("cached artifact checksum mismatch".to_string());
    }

    Ok(Some(CachedArtifact {
        path: paths.artifact_path.clone(),
        size_bytes: artifact_size,
    }))
}

fn persist_cached_artifact(
    artifact_temp_path: &Path,
    paths: &ArtifactCachePaths,
    cache_key: &str,
    artifact_size: u64,
) -> Result<(), String> {
    if let Some(parent) = paths.artifact_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
    }

    let artifact_sha = compute_file_hash(artifact_temp_path).map_err(|e| {
        format!(
            "Failed to hash built artifact {}: {e}",
            artifact_temp_path.display()
        )
    })?;
    let metadata = ArtifactCacheMetadata {
        schema_version: ARTIFACT_CACHE_SCHEMA_VERSION,
        cache_key: cache_key.to_string(),
        artifact_sha256: artifact_sha,
        artifact_size,
    };
    let metadata_bytes = serde_json::to_vec_pretty(&metadata)
        .map_err(|e| format!("Failed to serialize artifact cache metadata: {e}"))?;
    let metadata_temp_path = artifact_cache_temp_path(&paths.metadata_path)?;
    std::fs::write(&metadata_temp_path, metadata_bytes).map_err(|e| {
        format!(
            "Failed to write artifact cache metadata {}: {e}",
            metadata_temp_path.display()
        )
    })?;

    std::fs::rename(artifact_temp_path, &paths.artifact_path).map_err(|e| {
        format!(
            "Failed to move artifact {} to {}: {e}",
            artifact_temp_path.display(),
            paths.artifact_path.display()
        )
    })?;
    if let Err(e) = std::fs::rename(&metadata_temp_path, &paths.metadata_path) {
        let _ = std::fs::remove_file(&paths.artifact_path);
        return Err(format!(
            "Failed to move cache metadata {} to {}: {e}",
            metadata_temp_path.display(),
            paths.metadata_path.display()
        ));
    }

    Ok(())
}

fn acquire_artifact_cache_lock(lock_path: &Path) -> Result<ArtifactCacheLockGuard, String> {
    acquire_artifact_cache_lock_with_options(
        lock_path,
        Duration::from_secs(ARTIFACT_CACHE_LOCK_TIMEOUT_SECS),
        Duration::from_secs(ARTIFACT_CACHE_STALE_LOCK_SECS),
    )
}

fn acquire_artifact_cache_lock_with_options(
    lock_path: &Path,
    timeout: Duration,
    stale_after: Duration,
) -> Result<ArtifactCacheLockGuard, String> {
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create {}: {e}", parent.display()))?;
    }

    let start = Instant::now();
    loop {
        match std::fs::create_dir(lock_path) {
            Ok(()) => {
                return Ok(ArtifactCacheLockGuard {
                    lock_path: lock_path.to_path_buf(),
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if stale_after > Duration::from_secs(0)
                    && let Ok(metadata) = std::fs::metadata(lock_path)
                    && let Ok(modified_at) = metadata.modified()
                    && let Ok(age) = modified_at.elapsed()
                    && age > stale_after
                {
                    let _ = std::fs::remove_dir_all(lock_path);
                    continue;
                }

                if start.elapsed() >= timeout {
                    return Err(format!(
                        "Timed out waiting for artifact cache lock {}",
                        lock_path.display()
                    ));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(error) => {
                return Err(format!(
                    "Failed to acquire artifact cache lock {}: {error}",
                    lock_path.display()
                ));
            }
        }
    }
}

async fn build_target_artifacts(
    project_dir: &Path,
    cache_dir: &Path,
    build_workspace_root: &Path,
    source_archive_path: &Path,
    source_hash: &str,
    version: &str,
    app_subdir: &str,
    runtime_name: &str,
    runtime_tool: &str,
    main: &str,
    server_targets: &[(String, ServerTarget)],
    preset: &BuildPreset,
    preset_source: &ResolvedPresetSource,
    custom_stages: &[BuildStage],
    include_patterns: &[String],
    exclude_patterns: &[String],
    asset_roots: &[String],
) -> Result<HashMap<String, PathBuf>, String> {
    let unique_targets: BTreeSet<String> = server_targets
        .iter()
        .map(|(_, target)| target.label())
        .collect();
    let use_docker_build = should_use_docker_build(preset);
    let mut artifacts = HashMap::new();

    for target_label in unique_targets {
        let target_build = resolve_build_target(preset, &target_label)?;
        let use_local_build_spinners = should_use_local_build_spinners(output::is_interactive());
        let stage_summary = summarize_build_stages(&target_build, custom_stages);
        if !stage_summary.is_empty() {
            output::muted(&format!(
                "Build stages for {}: {}",
                target_label,
                stage_summary.join(" -> ")
            ));
        }
        std::fs::create_dir_all(build_workspace_root)
            .map_err(|e| format!("Failed to create {}: {e}", build_workspace_root.display()))?;
        let workspace = build_workspace_root.join(format!("work-{}-{}", version, target_label));
        if workspace.exists() {
            std::fs::remove_dir_all(&workspace)
                .map_err(|e| format!("Failed to clear {}: {e}", workspace.display()))?;
        }
        std::fs::create_dir_all(&workspace)
            .map_err(|e| format!("Failed to create {}: {e}", workspace.display()))?;
        BuildExecutor::extract_archive(source_archive_path, &workspace).map_err(|e| {
            format!(
                "Failed to extract source archive for {}: {}",
                target_label, e
            )
        })?;

        let runtime_version = if use_docker_build {
            resolve_runtime_version_with_docker_probe(
                &workspace,
                app_subdir,
                &target_label,
                runtime_tool,
                &target_build,
            )?
        } else {
            resolve_runtime_version_from_workspace(&workspace, app_subdir, runtime_tool)?
        };

        let cache_key = build_artifact_cache_key(
            source_hash,
            runtime_name,
            runtime_tool,
            &runtime_version,
            use_docker_build,
            &target_label,
            preset_source,
            &target_build,
            custom_stages,
            include_patterns,
            exclude_patterns,
            asset_roots,
            app_subdir,
        )?;
        let cache_paths = artifact_cache_paths(cache_dir, &target_label, &cache_key);
        let _cache_lock = acquire_artifact_cache_lock(&cache_paths.lock_path)?;

        match load_valid_cached_artifact(&cache_paths, &cache_key) {
            Ok(Some(cached)) => {
                output::success(&format!(
                    "Artifact cache hit for {}: {} ({})",
                    target_label,
                    format_path_relative_to(project_dir, &cached.path),
                    format_size(cached.size_bytes)
                ));
                artifacts.insert(target_label, cached.path);
                let _ = std::fs::remove_dir_all(&workspace);
                continue;
            }
            Ok(None) => {}
            Err(error) => {
                output::warning(&format!(
                    "Artifact cache entry for {} is invalid ({}); rebuilding.",
                    target_label, error
                ));
                remove_cached_artifact_files(&cache_paths);
            }
        }

        let build_result = (|| -> Result<u64, String> {
            let build_label = format!("Building artifact for {}...", target_label);
            if use_local_build_spinners {
                output::with_spinner(build_label.as_str(), || {
                    run_target_build(
                        &workspace,
                        app_subdir,
                        &target_label,
                        runtime_tool,
                        use_docker_build,
                        &target_build,
                        custom_stages,
                    )
                })
                .map_err(|e| format!("Failed to render artifact build spinner: {e}"))??;
            } else {
                output::step(&build_label);
                run_target_build(
                    &workspace,
                    app_subdir,
                    &target_label,
                    runtime_tool,
                    use_docker_build,
                    &target_build,
                    custom_stages,
                )?;
            }
            materialize_runtime_tool_version(
                &workspace,
                app_subdir,
                runtime_tool,
                &runtime_version,
            )?;
            output::success(&format_build_completed_message(&target_label));

            let prepare_label = format_prepare_artifact_message(&target_label);
            if use_local_build_spinners {
                output::with_spinner(prepare_label.as_str(), || {
                    package_target_artifact(
                        &workspace,
                        app_subdir,
                        asset_roots,
                        include_patterns,
                        exclude_patterns,
                        &cache_paths,
                        &cache_key,
                        main,
                        &target_label,
                    )
                })
                .map_err(|e| format!("Failed to render artifact preparation spinner: {e}"))?
            } else {
                output::step(&prepare_label);
                package_target_artifact(
                    &workspace,
                    app_subdir,
                    asset_roots,
                    include_patterns,
                    exclude_patterns,
                    &cache_paths,
                    &cache_key,
                    main,
                    &target_label,
                )
            }
        })();
        let _ = std::fs::remove_dir_all(&workspace);
        let artifact_size = build_result?;

        output::success(&format!(
            "Artifact ready for {}: {} ({})",
            target_label,
            format_path_relative_to(project_dir, &cache_paths.artifact_path),
            format_size(artifact_size)
        ));
        artifacts.insert(target_label, cache_paths.artifact_path.clone());
    }

    Ok(artifacts)
}

fn run_target_build(
    workspace: &Path,
    app_subdir: &str,
    target_label: &str,
    runtime_tool: &str,
    use_docker_build: bool,
    target_build: &crate::build::BuildPresetTarget,
    custom_stages: &[BuildStage],
) -> Result<(), String> {
    if use_docker_build {
        let stage_commands = container_stage_commands(custom_stages);
        run_container_build(
            workspace,
            app_subdir,
            target_label,
            runtime_tool,
            target_build,
            &stage_commands,
        )?;
    } else {
        run_local_build(workspace, app_subdir, target_build, custom_stages)?;
    }
    Ok(())
}

fn run_local_build(
    workspace: &Path,
    app_subdir: &str,
    target_build: &crate::build::BuildPresetTarget,
    custom_stages: &[BuildStage],
) -> Result<(), String> {
    let app_dir = workspace_app_dir(workspace, app_subdir);
    if !app_dir.is_dir() {
        return Err(format!(
            "App directory '{}' does not exist inside build workspace",
            app_dir.display()
        ));
    }

    let app_subdir_value = app_subdir.replace('\\', "/");
    let app_dir_value = app_dir.to_string_lossy().to_string();
    let has_preset_stage = target_build
        .install
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .is_some()
        || target_build
            .build
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .is_some();
    if !has_preset_stage && custom_stages.is_empty() {
        return Err(
            "Build preset did not define install/build commands and no build stages were configured"
                .to_string(),
        );
    }
    let mut stage_number = if has_preset_stage { 2usize } else { 1usize };

    let run_shell =
        |cwd: &Path, command: &str, phase: &str, stage_label: &str| -> Result<(), String> {
            let output = std::process::Command::new("sh")
                .args(["-lc", command])
                .current_dir(cwd)
                .env("TAKO_APP_SUBDIR", &app_subdir_value)
                .env("TAKO_APP_DIR", &app_dir_value)
                .output()
                .map_err(|e| format!("Failed to run local {stage_label} {phase} command: {e}"))?;
            if output.status.success() {
                return Ok(());
            }
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let detail = if stderr.is_empty() { stdout } else { stderr };
            Err(format!(
                "Local {stage_label} {phase} command failed: {detail}"
            ))
        };

    let preset_stage_label = "stage 1 (preset)";
    if let Some(install) = target_build
        .install
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        run_shell(workspace, install, "install", preset_stage_label)?;
    }
    if let Some(build) = target_build
        .build
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        run_shell(&app_dir, build, "run", preset_stage_label)?;
    }

    for stage in custom_stages {
        let stage_label = format_stage_label(stage_number, stage.name.as_deref());
        let stage_cwd = resolve_stage_working_dir_for_local_build(
            &app_dir,
            stage.working_dir.as_deref(),
            &stage_label,
        )?;
        if let Some(install) = stage
            .install
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            run_shell(&stage_cwd, install, "install", &stage_label)?;
        }
        let run_command = stage.run.trim();
        if run_command.is_empty() {
            return Err(format!("Local {stage_label} run command is empty"));
        }
        run_shell(&stage_cwd, run_command, "run", &stage_label)?;
        stage_number += 1;
    }

    Ok(())
}

fn container_stage_commands(stages: &[BuildStage]) -> Vec<BuildStageCommand> {
    stages
        .iter()
        .map(|stage| BuildStageCommand {
            name: stage.name.clone(),
            working_dir: stage.working_dir.clone(),
            install: stage.install.clone(),
            run: stage.run.clone(),
        })
        .collect()
}

fn format_stage_label(stage_number: usize, stage_name: Option<&str>) -> String {
    match stage_name.map(str::trim).filter(|value| !value.is_empty()) {
        Some(name) => format!("stage {stage_number} ({name})"),
        None => format!("stage {stage_number}"),
    }
}

fn summarize_build_stages(
    target_build: &crate::build::BuildPresetTarget,
    custom_stages: &[BuildStage],
) -> Vec<String> {
    let mut labels = Vec::new();
    let has_preset_stage = target_build
        .install
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .is_some()
        || target_build
            .build
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some();
    if has_preset_stage {
        labels.push("stage 1 (preset)".to_string());
    }

    let mut stage_number = if has_preset_stage { 2 } else { 1 };
    for stage in custom_stages {
        labels.push(format_stage_label(stage_number, stage.name.as_deref()));
        stage_number += 1;
    }
    labels
}

fn resolve_stage_working_dir_for_local_build(
    app_dir: &Path,
    working_dir: Option<&str>,
    stage_label: &str,
) -> Result<PathBuf, String> {
    let Some(working_dir) = working_dir.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(app_dir.to_path_buf());
    };
    let stage_dir = app_dir.join(working_dir);
    if !stage_dir.is_dir() {
        return Err(format!(
            "Local {stage_label} working directory '{}' was not found at '{}'",
            working_dir,
            stage_dir.display()
        ));
    }
    Ok(stage_dir)
}

fn materialize_runtime_tool_version(
    workspace: &Path,
    app_subdir: &str,
    runtime_tool: &str,
    runtime_version: &str,
) -> Result<(), String> {
    let app_dir = workspace_app_dir(workspace, app_subdir);
    if !app_dir.is_dir() {
        return Err(format!(
            "App directory '{}' does not exist inside build workspace",
            app_dir.display()
        ));
    }

    write_runtime_proto_tools_file(workspace, &app_dir, runtime_tool, runtime_version)?;
    let _ = std::fs::remove_file(app_dir.join(RUNTIME_VERSION_OUTPUT_FILE));
    Ok(())
}

fn write_runtime_proto_tools_file(
    workspace_root: &Path,
    app_dir: &Path,
    runtime_name: &str,
    version: &str,
) -> Result<(), String> {
    let proto_tools_path = app_dir.join(PROTO_TOOLS_FILE);
    let workspace_tools_path = workspace_root.join(PROTO_TOOLS_FILE);
    let mut table = if proto_tools_path.is_file() {
        match std::fs::read_to_string(&proto_tools_path)
            .map_err(|e| format!("Failed to read {}: {e}", proto_tools_path.display()))?
            .parse::<toml::Table>()
        {
            Ok(existing) => existing,
            Err(_) => toml::Table::new(),
        }
    } else if app_dir != workspace_root && workspace_tools_path.is_file() {
        match std::fs::read_to_string(&workspace_tools_path)
            .map_err(|e| format!("Failed to read {}: {e}", workspace_tools_path.display()))?
            .parse::<toml::Table>()
        {
            Ok(existing) => existing,
            Err(_) => toml::Table::new(),
        }
    } else {
        toml::Table::new()
    };
    table.insert(
        runtime_name.to_string(),
        toml::Value::String(version.to_string()),
    );
    let rendered = toml::to_string(&table)
        .map_err(|e| format!("Failed to render {}: {e}", proto_tools_path.display()))?;
    std::fs::write(&proto_tools_path, rendered)
        .map_err(|e| format!("Failed to write {}: {e}", proto_tools_path.display()))?;
    Ok(())
}

fn resolve_runtime_version_with_docker_probe(
    workspace: &Path,
    app_subdir: &str,
    target_label: &str,
    runtime_tool: &str,
    target_build: &crate::build::BuildPresetTarget,
) -> Result<String, String> {
    let probe_target = crate::build::BuildPresetTarget {
        builder_image: target_build.builder_image.clone(),
        install: None,
        build: Some("true".to_string()),
    };
    run_container_build(
        workspace,
        app_subdir,
        target_label,
        runtime_tool,
        &probe_target,
        &[],
    )?;
    read_runtime_version_output(workspace, app_subdir, runtime_tool)
}

fn resolve_runtime_version_from_workspace(
    workspace: &Path,
    app_subdir: &str,
    runtime_tool: &str,
) -> Result<String, String> {
    let app_dir = workspace_app_dir(workspace, app_subdir);
    if !app_dir.is_dir() {
        return Err(format!(
            "App directory '{}' does not exist inside build workspace",
            app_dir.display()
        ));
    }
    if let Some(version) =
        resolve_runtime_version_with_local_proto(workspace, &app_dir, runtime_tool)?
    {
        return Ok(version);
    }

    let app_tools = app_dir.join(PROTO_TOOLS_FILE);
    let workspace_tools = workspace.join(PROTO_TOOLS_FILE);

    let spec = if app_tools.is_file() {
        read_runtime_version_spec_from_prototools(&app_tools, runtime_tool)?
    } else if workspace_tools.is_file() {
        read_runtime_version_spec_from_prototools(&workspace_tools, runtime_tool)?
    } else {
        None
    };

    Ok(spec.unwrap_or_else(|| "latest".to_string()))
}

fn resolve_runtime_version_with_local_proto(
    workspace: &Path,
    app_dir: &Path,
    runtime_tool: &str,
) -> Result<Option<String>, String> {
    #[cfg(test)]
    {
        let _ = workspace;
        let _ = app_dir;
        let _ = runtime_tool;
        return Ok(None);
    }

    #[cfg(not(test))]
    {
        let app_dir_str = app_dir.to_string_lossy().to_string();
        let workspace_str = workspace.to_string_lossy().to_string();
        let command = format!(
            "cd {} && proto install --yes >/dev/null 2>&1 || true; proto run {} -- --version",
            shell_single_quote(&app_dir_str),
            shell_single_quote(runtime_tool)
        );
        let output = std::process::Command::new("sh")
            .args(["-lc", &command])
            .current_dir(workspace)
            .output()
            .map_err(|e| {
                format!(
                    "Failed to run local runtime version probe with proto in '{}': {e}",
                    workspace_str
                )
            })?;
        if !output.status.success() {
            return Ok(None);
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let version = stdout
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(str::to_string);
        Ok(version)
    }
}

fn read_runtime_version_spec_from_prototools(
    path: &Path,
    runtime_tool: &str,
) -> Result<Option<String>, String> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read {}: {e}", path.display()))?;
    let table = match raw.parse::<toml::Table>() {
        Ok(value) => value,
        Err(_) => return Ok(None),
    };
    Ok(table
        .get(runtime_tool)
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string()))
}

fn read_runtime_version_output(
    workspace: &Path,
    app_subdir: &str,
    runtime_tool: &str,
) -> Result<String, String> {
    let app_dir = workspace_app_dir(workspace, app_subdir);
    let runtime_version_path = app_dir.join(RUNTIME_VERSION_OUTPUT_FILE);
    if !runtime_version_path.is_file() {
        return Err(format!(
            "Runtime version probe for '{}' did not produce {}",
            runtime_tool,
            runtime_version_path.display()
        ));
    }
    let raw = std::fs::read_to_string(&runtime_version_path)
        .map_err(|e| format!("Failed to read {}: {e}", runtime_version_path.display()))?;
    let version = raw
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| {
            format!(
                "Runtime version probe for '{}' returned empty output",
                runtime_tool
            )
        })?
        .to_string();
    let _ = std::fs::remove_file(runtime_version_path);
    Ok(version)
}

fn merge_assets_locally(
    workspace_root: &Path,
    app_subdir: &str,
    asset_roots: &[String],
) -> Result<(), String> {
    if asset_roots.is_empty() {
        return Ok(());
    }

    let app_dir = workspace_app_dir(workspace_root, app_subdir);
    if !app_dir.is_dir() {
        return Err(format!(
            "App directory '{}' does not exist inside build workspace",
            app_dir.display()
        ));
    }

    let public_dir = app_dir.join("public");
    std::fs::create_dir_all(&public_dir)
        .map_err(|e| format!("Failed to create {}: {e}", public_dir.display()))?;

    for asset_root in asset_roots {
        if asset_root == "public" {
            continue;
        }
        let src = app_dir.join(asset_root);
        if !src.is_dir() {
            return Err(format!(
                "Configured asset directory '{}' not found after build.",
                asset_root
            ));
        }
        copy_dir_contents(&src, &public_dir)?;
    }

    Ok(())
}

fn workspace_app_dir(workspace_root: &Path, app_subdir: &str) -> PathBuf {
    if app_subdir.is_empty() {
        workspace_root.to_path_buf()
    } else {
        workspace_root.join(app_subdir)
    }
}

fn validate_main_exists_after_build(
    workspace_root: &Path,
    app_subdir: &str,
    main: &str,
) -> Result<(), String> {
    let app_dir = workspace_app_dir(workspace_root, app_subdir);
    let main_path = app_dir.join(main);
    if main_path.is_file() {
        return Ok(());
    }

    Err(format!(
        "Deploy entrypoint '{}' was not found after build at '{}'. Ensure the build output contains this file or update `main` in tako.toml/preset.",
        main,
        main_path.display()
    ))
}

fn package_target_artifact(
    workspace: &Path,
    app_subdir: &str,
    asset_roots: &[String],
    include_patterns: &[String],
    exclude_patterns: &[String],
    cache_paths: &ArtifactCachePaths,
    cache_key: &str,
    main: &str,
    target_label: &str,
) -> Result<u64, String> {
    merge_assets_locally(workspace, app_subdir, asset_roots)?;
    let app_dir = workspace_app_dir(workspace, app_subdir);
    if !app_dir.is_dir() {
        return Err(format!(
            "App directory '{}' does not exist inside build workspace",
            app_dir.display()
        ));
    }
    validate_main_exists_after_build(workspace, app_subdir, main)?;

    let artifact_temp_path = artifact_cache_temp_path(&cache_paths.artifact_path)?;
    let artifact_size = create_filtered_archive_with_prefix(
        workspace,
        &artifact_temp_path,
        include_patterns,
        exclude_patterns,
        None,
    )
    .map_err(|e| format!("Failed to create artifact for {}: {}", target_label, e))?;

    if let Err(error) =
        persist_cached_artifact(&artifact_temp_path, cache_paths, cache_key, artifact_size)
    {
        let _ = std::fs::remove_file(&artifact_temp_path);
        let _ = std::fs::remove_file(&cache_paths.metadata_path);
        return Err(format!(
            "Failed to persist cached artifact for {}: {}",
            target_label, error
        ));
    }

    Ok(artifact_size)
}

fn copy_dir_contents(src: &Path, dst: &Path) -> Result<(), String> {
    for entry in
        std::fs::read_dir(src).map_err(|e| format!("Failed to read {}: {e}", src.display()))?
    {
        let entry =
            entry.map_err(|e| format!("Failed to read dir entry in {}: {e}", src.display()))?;
        let path = entry.path();
        let target = dst.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|e| format!("Failed to inspect {}: {e}", path.display()))?;
        if file_type.is_dir() {
            std::fs::create_dir_all(&target)
                .map_err(|e| format!("Failed to create {}: {e}", target.display()))?;
            copy_dir_contents(&path, &target)?;
        } else if file_type.is_file() {
            std::fs::copy(&path, &target).map_err(|e| {
                format!(
                    "Failed to copy {} to {}: {e}",
                    path.display(),
                    target.display()
                )
            })?;
        }
    }
    Ok(())
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

fn should_use_local_build_spinners(interactive: bool) -> bool {
    interactive
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

fn normalize_asset_root(asset_root: &str) -> Result<String, String> {
    let trimmed = asset_root.trim();
    if trimmed.is_empty() {
        return Err("Configured assets entry cannot be empty".to_string());
    }

    let path = Path::new(trimmed);
    if path.is_absolute() {
        return Err(format!(
            "Configured assets entry '{}' must be relative to project root",
            asset_root
        ));
    }

    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(format!(
            "Configured assets entry '{}' must not contain '..'",
            asset_root
        ));
    }

    Ok(trimmed.replace('\\', "/"))
}

fn remote_release_archive_path(release_dir: &str) -> String {
    format!("{release_dir}/artifacts.tar.gz")
}

fn build_remote_extract_archive_command(release_dir: &str, remote_archive: &str) -> String {
    format!(
        "tar -xzf {} -C {} && rm -f {}",
        shell_single_quote(remote_archive),
        shell_single_quote(release_dir),
        shell_single_quote(remote_archive)
    )
}

fn build_remote_write_manifest_command(app_json_path: &str, encoded: &str) -> String {
    format!(
        "printf '%s' '{}' | base64 -d | tee {} >/dev/null",
        encoded,
        shell_single_quote(app_json_path)
    )
}

/// Deploy to a single server
async fn deploy_to_server(
    config: &DeployConfig,
    server: &ServerEntry,
    archive_path: &Path,
    target_label: &str,
    instances: u8,
    idle_timeout: u32,
    use_spinner: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let ssh_config = SshConfig::from_server(&server.host, server.port);
    let ssh_keys_dir = ssh_config.keys_directory();
    let mut ssh = SshClient::new(ssh_config);
    run_deploy_step("Connecting...", use_spinner, ssh.connect()).await?;
    let archive_size_bytes = std::fs::metadata(archive_path)?.len();

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
    let release_app_dir = config.release_app_dir();
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
            ssh.mkdir(&release_app_dir).await?;
            ssh.mkdir(&config.shared_dir()).await?;
            Ok::<(), SshError>(())
        })
        .await?;

        // Upload target-specific archive artifact.
        let remote_archive = remote_release_archive_path(&release_dir);
        run_deploy_step(
            "Uploading artifact...",
            use_spinner,
            upload_via_scp(
                archive_path,
                &server.host,
                server.port,
                &remote_archive,
                &ssh_keys_dir,
            ),
        )
        .await?;

        // Extract archive directly into release root.
        let extract_cmd = build_remote_extract_archive_command(&release_dir, &remote_archive);
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

        // Finalize runtime manifest using the resolved deploy entrypoint.
        let resolved_main = config.main.clone();
        run_deploy_step("Preparing runtime manifest...", use_spinner, async {
            let main = resolved_main.clone();
            let app_json = serde_json::to_vec_pretty(&serde_json::json!({
                "runtime": config.runtime,
                "main": main,
                "install": config.deploy_install.clone(),
                "start": config.deploy_start.clone(),
            }))
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
            let encoded =
                base64::Engine::encode(&base64::engine::general_purpose::STANDARD, app_json);
            let app_json_path = config.release_app_manifest_path();
            let write_manifest_cmd = build_remote_write_manifest_command(&app_json_path, &encoded);
            ssh.exec_checked(&write_manifest_cmd).await?;
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        })
        .await?;
        output::muted(&format!(
            "Deploy main: {} (artifact target: {})",
            resolved_main, target_label
        ));

        // Send deploy command to tako-server.
        let cmd = Command::Deploy {
            app: config.app_name.clone(),
            version: config.version.clone(),
            path: release_app_dir.clone(),
            routes: config.routes.clone(),
            env: config.env_vars.clone(),
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
    let out = ssh.exec(&tako_process_probe_command()).await?;
    Ok(out.stdout.trim() == "yes")
}

fn tako_process_probe_command() -> String {
    // BusyBox `pgrep -x` can miss full-path command names (`/usr/local/bin/tako-server`).
    // Use `-f` with an anchored pattern to match both bare and full-path invocations.
    "pgrep -f '(^|/)tako-server([[:space:]]|$)' >/dev/null 2>&1 && echo yes || echo no".to_string()
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

fn format_path_relative_to(project_dir: &Path, path: &Path) -> String {
    match path.strip_prefix(project_dir) {
        Ok(relative) if !relative.as_os_str().is_empty() => relative.display().to_string(),
        Ok(_) => ".".to_string(),
        Err(_) => path.display().to_string(),
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
    fn resolve_build_preset_ref_prefers_tako_toml_override() {
        let temp = TempDir::new().unwrap();
        let config = TakoToml {
            preset: Some("github:owner/repo/presets/bun.toml@abc1234".to_string()),
            ..Default::default()
        };

        assert_eq!(
            resolve_build_preset_ref(temp.path(), &config).unwrap(),
            "github:owner/repo/presets/bun.toml@abc1234"
        );
    }

    #[test]
    fn resolve_build_preset_ref_qualifies_runtime_local_alias() {
        let temp = TempDir::new().unwrap();
        let config = TakoToml {
            runtime: Some("bun".to_string()),
            preset: Some("tanstack-start".to_string()),
            ..Default::default()
        };

        assert_eq!(
            resolve_build_preset_ref(temp.path(), &config).unwrap(),
            "bun/tanstack-start"
        );
    }

    #[test]
    fn resolve_build_preset_ref_errors_when_runtime_is_unknown_for_local_alias() {
        let temp = TempDir::new().unwrap();
        let config = TakoToml {
            preset: Some("tanstack-start".to_string()),
            ..Default::default()
        };

        let err = resolve_build_preset_ref(temp.path(), &config).unwrap_err();
        assert!(err.contains("Cannot resolve preset"));
    }

    #[test]
    fn resolve_build_preset_ref_falls_back_to_detected_adapter_default() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("package.json"), r#"{"name":"demo"}"#).unwrap();
        let config = TakoToml::default();
        assert_eq!(
            resolve_build_preset_ref(temp.path(), &config).unwrap(),
            "node"
        );
    }

    #[test]
    fn resolve_build_preset_ref_uses_build_adapter_override_when_preset_is_missing() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("package.json"), r#"{"name":"demo"}"#).unwrap();
        let config = TakoToml {
            runtime: Some("deno".to_string()),
            ..Default::default()
        };
        assert_eq!(
            resolve_build_preset_ref(temp.path(), &config).unwrap(),
            "deno"
        );
    }

    #[test]
    fn resolve_build_preset_ref_rejects_unknown_build_adapter_override() {
        let temp = TempDir::new().unwrap();
        let config = TakoToml {
            runtime: Some("python".to_string()),
            ..Default::default()
        };
        let err = resolve_build_preset_ref(temp.path(), &config).unwrap_err();
        assert!(err.contains("Invalid runtime"));
    }

    #[test]
    fn resolve_effective_build_adapter_uses_preset_family_when_detection_is_unknown() {
        let temp = TempDir::new().unwrap();
        let config = TakoToml::default();

        let adapter = resolve_effective_build_adapter(temp.path(), &config, "bun").unwrap();
        assert_eq!(adapter, BuildAdapter::Bun);
    }

    #[test]
    fn resolve_effective_build_adapter_prefers_runtime_override() {
        let temp = TempDir::new().unwrap();
        let config = TakoToml {
            runtime: Some("node".to_string()),
            ..Default::default()
        };

        let adapter =
            resolve_effective_build_adapter(temp.path(), &config, "tanstack-start").unwrap();
        assert_eq!(adapter, BuildAdapter::Node);
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
        let config = TakoToml {
            name: Some("test-app".to_string()),
            ..Default::default()
        };
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
        std::fs::write(
            temp_dir.path().join("tako.toml"),
            r#"
name = "test-app"
"#,
        )
        .unwrap();

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
            remote_base: "/opt/tako/apps/my-app".to_string(),
            routes: vec![],
            env_vars: HashMap::new(),
            app_subdir: "examples/bun".to_string(),
            runtime: "bun".to_string(),
            main: "index.ts".to_string(),
            deploy_install: Some("bun install --production --frozen-lockfile".to_string()),
            deploy_start: vec!["bun".to_string(), "run".to_string(), "index.ts".to_string()],
        };
        assert_eq!(cfg.release_dir(), "/opt/tako/apps/my-app/releases/v1");
        assert_eq!(
            cfg.release_app_dir(),
            "/opt/tako/apps/my-app/releases/v1/examples/bun"
        );
        assert_eq!(
            cfg.release_app_manifest_path(),
            "/opt/tako/apps/my-app/releases/v1/examples/bun/app.json"
        );
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
    fn artifact_progress_helpers_render_build_and_packaging_steps() {
        assert_eq!(
            format_build_completed_message("linux-aarch64-musl"),
            "Build completed for linux-aarch64-musl"
        );
        assert_eq!(
            format_prepare_artifact_message("linux-aarch64-musl"),
            "Preparing artifact for linux-aarch64-musl..."
        );
    }

    #[test]
    fn should_use_per_server_spinners_only_for_single_interactive_target() {
        assert!(should_use_per_server_spinners(1, true));
        assert!(!should_use_per_server_spinners(2, true));
        assert!(!should_use_per_server_spinners(1, false));
    }

    #[test]
    fn tako_process_probe_command_uses_busybox_safe_pgrep_f_pattern() {
        let cmd = tako_process_probe_command();
        assert!(cmd.contains("pgrep -f"));
        assert!(cmd.contains("(^|/)tako-server([[:space:]]|$)"));
        assert!(!cmd.contains("pgrep -x"));
    }

    #[test]
    fn should_use_local_build_spinners_only_when_interactive() {
        assert!(should_use_local_build_spinners(true));
        assert!(!should_use_local_build_spinners(false));
    }

    #[test]
    fn format_size_uses_expected_units() {
        assert_eq!(format_size(999), "999 bytes");
        assert_eq!(format_size(1024), "1.00 KB");
        assert_eq!(format_size(1024 * 1024), "1.00 MB");
        assert_eq!(format_size(1024 * 1024 * 1024), "1.00 GB");
    }

    #[test]
    fn format_path_relative_to_returns_project_relative_path_when_possible() {
        let project = Path::new("/repo/examples/js/bun");
        let artifact = Path::new("/repo/examples/js/bun/.tako/artifacts/a.tar.gz");
        assert_eq!(
            format_path_relative_to(project, artifact),
            ".tako/artifacts/a.tar.gz"
        );
    }

    #[test]
    fn format_path_relative_to_falls_back_to_absolute_when_outside_project() {
        let project = Path::new("/repo/examples/js/bun");
        let outside = Path::new("/tmp/a.tar.gz");
        assert_eq!(format_path_relative_to(project, outside), "/tmp/a.tar.gz");
    }

    #[test]
    fn build_deploy_command_env_vars_merges_manifest_build_and_secrets() {
        let manifest = DeployArchiveManifest {
            app_name: "my-app".to_string(),
            environment: "production".to_string(),
            version: "v123".to_string(),
            runtime: "bun".to_string(),
            main: "server/index.ts".to_string(),
            env_vars: BTreeMap::from([
                ("A_KEY".to_string(), "a".to_string()),
                ("TAKO_ENV".to_string(), "production".to_string()),
            ]),
            secret_names: vec!["API_KEY".to_string(), "PATH_HINT".to_string()],
        };
        let mut secrets = HashMap::new();
        secrets.insert("API_KEY".to_string(), r#"ab\"cd"#.to_string());
        secrets.insert("PATH_HINT".to_string(), r#"C:\tmp\bin"#.to_string());

        let env = build_deploy_command_env_vars(&manifest, Some(&secrets));
        assert_eq!(env.get("TAKO_BUILD"), Some(&"v123".to_string()));
        assert_eq!(env.get("A_KEY"), Some(&"a".to_string()));
        assert_eq!(env.get("TAKO_ENV"), Some(&"production".to_string()));
        assert_eq!(env.get("API_KEY"), Some(&r#"ab\"cd"#.to_string()));
        assert_eq!(env.get("PATH_HINT"), Some(&r#"C:\tmp\bin"#.to_string()));
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
    fn archive_app_manifest_path_places_manifest_under_app_subdir() {
        assert_eq!(archive_app_manifest_path(""), "app.json");
        assert_eq!(archive_app_manifest_path("apps/web"), "apps/web/app.json");
    }

    #[test]
    fn resolve_app_subdir_uses_source_root_prefix() {
        let source_root = Path::new("/repo");
        let project_dir = Path::new("/repo/apps/web");
        let subdir = resolve_app_subdir(source_root, project_dir).unwrap();
        assert_eq!(subdir, "apps/web");
    }

    #[test]
    fn source_bundle_root_falls_back_to_project_dir_without_git() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().join("app");
        std::fs::create_dir_all(&project_dir).unwrap();
        assert_eq!(source_bundle_root(&project_dir), project_dir);
    }

    #[test]
    fn normalize_asset_root_rejects_invalid_paths() {
        assert!(normalize_asset_root(" ").is_err());
        assert!(normalize_asset_root("/tmp/assets").is_err());
        assert!(normalize_asset_root("../assets").is_err());
    }

    #[test]
    fn resolve_build_target_uses_preset_build_commands() {
        let preset = BuildPreset {
            name: "bun".to_string(),
            main: None,
            builder_image: None,
            build: crate::build::BuildPresetBuild {
                exclude: vec![],
                install: Some("bun install".to_string()),
                build: Some("bun run build".to_string()),
                targets: vec!["linux-x86_64-glibc".to_string()],
                container: true,
                ..crate::build::BuildPresetBuild::default()
            },
            dev: vec![],
            install: None,
            start: vec![],
            targets: HashMap::new(),
            target_defaults: crate::build::BuildPresetTargetDefaults::default(),
            assets: vec![],
        };

        let resolved = resolve_build_target(&preset, "linux-x86_64-glibc").unwrap();
        assert_eq!(resolved.builder_image.as_deref(), None);
        assert_eq!(resolved.install.as_deref(), Some("bun install"));
        assert_eq!(resolved.build.as_deref(), Some("bun run build"));
    }

    #[test]
    fn resolve_build_target_allows_any_target_when_targets_are_not_configured() {
        let preset = BuildPreset {
            name: "bun".to_string(),
            main: None,
            builder_image: None,
            build: crate::build::BuildPresetBuild::default(),
            dev: vec![],
            install: None,
            start: vec![],
            targets: HashMap::new(),
            target_defaults: crate::build::BuildPresetTargetDefaults::default(),
            assets: vec![],
        };

        let resolved = resolve_build_target(&preset, "linux-aarch64-musl").unwrap();
        assert_eq!(resolved.builder_image.as_deref(), None);
        assert_eq!(resolved.install.as_deref(), None);
        assert_eq!(resolved.build.as_deref(), None);
    }

    #[test]
    fn resolve_build_target_errors_when_target_is_not_listed() {
        let preset = BuildPreset {
            name: "bun".to_string(),
            main: None,
            builder_image: None,
            build: crate::build::BuildPresetBuild {
                exclude: vec![],
                install: Some("bun install".to_string()),
                build: Some("bun run build".to_string()),
                targets: vec!["linux-x86_64-glibc".to_string()],
                container: true,
                ..crate::build::BuildPresetBuild::default()
            },
            dev: vec![],
            install: None,
            start: vec![],
            targets: HashMap::new(),
            target_defaults: crate::build::BuildPresetTargetDefaults::default(),
            assets: vec![],
        };

        let err = resolve_build_target(&preset, "linux-aarch64-musl").unwrap_err();
        assert!(err.contains("does not define target 'linux-aarch64-musl'"));
    }

    #[test]
    fn artifact_cache_key_changes_when_build_inputs_change() {
        let resolved = crate::build::ResolvedPresetSource {
            preset_ref: "bun".to_string(),
            repo: "tako-sh/presets".to_string(),
            path: "presets/bun/bun.toml".to_string(),
            commit: "abc123def456".to_string(),
        };
        let target = crate::build::BuildPresetTarget {
            builder_image: Some("oven/bun:1.2".to_string()),
            install: Some("bun install".to_string()),
            build: Some("bun run build".to_string()),
        };
        let include_patterns = vec!["**/*".to_string()];
        let exclude_patterns: Vec<String> = vec![];
        let asset_roots = vec!["public".to_string()];
        let custom_stages: Vec<crate::config::BuildStage> = vec![];

        let baseline = build_artifact_cache_key(
            "source-hash-a",
            "bun",
            "bun",
            "1.3.9",
            true,
            "linux-x86_64-glibc",
            &resolved,
            &target,
            &custom_stages,
            &include_patterns,
            &exclude_patterns,
            &asset_roots,
            "apps/web",
        )
        .unwrap();

        let changed_source = build_artifact_cache_key(
            "source-hash-b",
            "bun",
            "bun",
            "1.3.9",
            true,
            "linux-x86_64-glibc",
            &resolved,
            &target,
            &custom_stages,
            &include_patterns,
            &exclude_patterns,
            &asset_roots,
            "apps/web",
        )
        .unwrap();
        assert_ne!(baseline, changed_source);

        let changed_runtime = build_artifact_cache_key(
            "source-hash-a",
            "node",
            "bun",
            "1.3.9",
            true,
            "linux-x86_64-glibc",
            &resolved,
            &target,
            &custom_stages,
            &include_patterns,
            &exclude_patterns,
            &asset_roots,
            "apps/web",
        )
        .unwrap();
        assert_ne!(baseline, changed_runtime);

        let changed_target = build_artifact_cache_key(
            "source-hash-a",
            "bun",
            "bun",
            "1.3.9",
            true,
            "linux-aarch64-glibc",
            &resolved,
            &target,
            &custom_stages,
            &include_patterns,
            &exclude_patterns,
            &asset_roots,
            "apps/web",
        )
        .unwrap();
        assert_ne!(baseline, changed_target);

        let changed_runtime_version = build_artifact_cache_key(
            "source-hash-a",
            "bun",
            "bun",
            "1.3.10",
            true,
            "linux-x86_64-glibc",
            &resolved,
            &target,
            &custom_stages,
            &include_patterns,
            &exclude_patterns,
            &asset_roots,
            "apps/web",
        )
        .unwrap();
        assert_ne!(baseline, changed_runtime_version);

        let mut changed_preset = resolved.clone();
        changed_preset.commit = "fff111aaa222".to_string();
        let changed_preset_commit = build_artifact_cache_key(
            "source-hash-a",
            "bun",
            "bun",
            "1.3.9",
            true,
            "linux-x86_64-glibc",
            &changed_preset,
            &target,
            &custom_stages,
            &include_patterns,
            &exclude_patterns,
            &asset_roots,
            "apps/web",
        )
        .unwrap();
        assert_ne!(baseline, changed_preset_commit);

        let mut changed_build_target = target.clone();
        changed_build_target.build = Some("bun run custom-build".to_string());
        let changed_script = build_artifact_cache_key(
            "source-hash-a",
            "bun",
            "bun",
            "1.3.9",
            true,
            "linux-x86_64-glibc",
            &resolved,
            &changed_build_target,
            &custom_stages,
            &include_patterns,
            &exclude_patterns,
            &asset_roots,
            "apps/web",
        )
        .unwrap();
        assert_ne!(baseline, changed_script);

        let changed_include = build_artifact_cache_key(
            "source-hash-a",
            "bun",
            "bun",
            "1.3.9",
            true,
            "linux-x86_64-glibc",
            &resolved,
            &target,
            &custom_stages,
            &["dist/**".to_string()],
            &exclude_patterns,
            &asset_roots,
            "apps/web",
        )
        .unwrap();
        assert_ne!(baseline, changed_include);

        let changed_stages = build_artifact_cache_key(
            "source-hash-a",
            "bun",
            "bun",
            "1.3.9",
            true,
            "linux-x86_64-glibc",
            &resolved,
            &target,
            &[crate::config::BuildStage {
                name: None,
                working_dir: None,
                install: None,
                run: "bun run build".to_string(),
            }],
            &include_patterns,
            &exclude_patterns,
            &asset_roots,
            "apps/web",
        )
        .unwrap();
        assert_ne!(baseline, changed_stages);
    }

    #[test]
    fn cached_artifact_round_trip_verifies_checksum_and_size() {
        let temp = TempDir::new().unwrap();
        let cache_dir = temp.path().join("artifacts");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let key = "abc123".to_string();
        let paths = artifact_cache_paths(&cache_dir, "linux-x86_64-glibc", &key);

        let artifact_tmp = cache_dir.join("artifact.tmp");
        std::fs::write(&artifact_tmp, b"hello artifact").unwrap();
        let size = std::fs::metadata(&artifact_tmp).unwrap().len();
        persist_cached_artifact(&artifact_tmp, &paths, &key, size).unwrap();

        let verified = load_valid_cached_artifact(&paths, &key).unwrap().unwrap();
        assert_eq!(verified.path, paths.artifact_path);
        assert_eq!(verified.size_bytes, size);
    }

    #[test]
    fn cached_artifact_verification_fails_on_checksum_mismatch() {
        let temp = TempDir::new().unwrap();
        let cache_dir = temp.path().join("artifacts");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let key = "abc123".to_string();
        let paths = artifact_cache_paths(&cache_dir, "linux-x86_64-glibc", &key);

        std::fs::write(&paths.artifact_path, b"hello artifact").unwrap();
        let bad_metadata = ArtifactCacheMetadata {
            schema_version: ARTIFACT_CACHE_SCHEMA_VERSION,
            cache_key: key.clone(),
            artifact_sha256: "deadbeef".to_string(),
            artifact_size: 14,
        };
        std::fs::write(
            &paths.metadata_path,
            serde_json::to_vec_pretty(&bad_metadata).unwrap(),
        )
        .unwrap();

        let err = load_valid_cached_artifact(&paths, &key).unwrap_err();
        assert!(err.contains("checksum mismatch"));
    }

    #[test]
    fn artifact_cache_lock_times_out_if_already_held() {
        let temp = TempDir::new().unwrap();
        let lock_path = temp.path().join("artifact.lock");
        let _held = acquire_artifact_cache_lock_with_options(
            &lock_path,
            std::time::Duration::from_secs(1),
            std::time::Duration::from_secs(60),
        )
        .unwrap();

        let err = acquire_artifact_cache_lock_with_options(
            &lock_path,
            std::time::Duration::from_millis(30),
            std::time::Duration::from_secs(60),
        )
        .unwrap_err();
        assert!(err.contains("Timed out waiting"));
    }

    #[test]
    fn cleanup_local_artifact_cache_prunes_old_source_and_target_archives() {
        let temp = TempDir::new().unwrap();
        let cache_dir = temp.path().join("artifacts");
        std::fs::create_dir_all(&cache_dir).unwrap();

        let old_source = cache_dir.join("v1-source.tar.gz");
        let new_source = cache_dir.join("v2-source.tar.gz");
        std::fs::write(&old_source, b"old source").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&new_source, b"new source").unwrap();

        let old_artifact = cache_dir.join("artifact-cache-linux-aarch64-glibc-old.tar.gz");
        let old_metadata = cache_dir.join("artifact-cache-linux-aarch64-glibc-old.json");
        let new_artifact = cache_dir.join("artifact-cache-linux-aarch64-glibc-new.tar.gz");
        let new_metadata = cache_dir.join("artifact-cache-linux-aarch64-glibc-new.json");
        std::fs::write(&old_artifact, b"old artifact").unwrap();
        std::fs::write(&old_metadata, b"{\"cache_key\":\"old\"}").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&new_artifact, b"new artifact").unwrap();
        std::fs::write(&new_metadata, b"{\"cache_key\":\"new\"}").unwrap();

        let summary = cleanup_local_artifact_cache(&cache_dir, 1, 1).unwrap();
        assert_eq!(
            summary,
            LocalArtifactCacheCleanupSummary {
                removed_source_archives: 1,
                removed_target_artifacts: 1,
                removed_target_metadata: 1,
            }
        );

        assert!(!old_source.exists());
        assert!(new_source.exists());
        assert!(!old_artifact.exists());
        assert!(!old_metadata.exists());
        assert!(new_artifact.exists());
        assert!(new_metadata.exists());
    }

    #[test]
    fn cleanup_local_artifact_cache_removes_orphan_target_metadata() {
        let temp = TempDir::new().unwrap();
        let cache_dir = temp.path().join("artifacts");
        std::fs::create_dir_all(&cache_dir).unwrap();

        let artifact = cache_dir.join("artifact-cache-linux-aarch64-glibc-live.tar.gz");
        let live_metadata = cache_dir.join("artifact-cache-linux-aarch64-glibc-live.json");
        let orphan_metadata = cache_dir.join("artifact-cache-linux-aarch64-glibc-orphan.json");
        std::fs::write(&artifact, b"live artifact").unwrap();
        std::fs::write(&live_metadata, b"{\"cache_key\":\"live\"}").unwrap();
        std::fs::write(&orphan_metadata, b"{\"cache_key\":\"orphan\"}").unwrap();

        let summary = cleanup_local_artifact_cache(&cache_dir, 10, 10).unwrap();
        assert_eq!(
            summary,
            LocalArtifactCacheCleanupSummary {
                removed_source_archives: 0,
                removed_target_artifacts: 0,
                removed_target_metadata: 1,
            }
        );

        assert!(artifact.exists());
        assert!(live_metadata.exists());
        assert!(!orphan_metadata.exists());
    }

    #[test]
    fn build_asset_roots_combines_and_deduplicates_preset_and_project_values() {
        let preset = BuildPreset {
            name: "bun".to_string(),
            main: None,
            builder_image: None,
            build: crate::build::BuildPresetBuild::default(),
            dev: vec![],
            install: None,
            start: vec![],
            targets: HashMap::new(),
            target_defaults: Default::default(),
            assets: vec!["public".to_string(), "dist/client".to_string()],
        };
        let config = TakoToml {
            build: crate::config::BuildConfig {
                include: vec![],
                exclude: vec![],
                assets: vec!["dist/client".to_string(), "assets/shared".to_string()],
                stages: vec![],
            },
            ..Default::default()
        };
        let merged = build_asset_roots(&preset, &config).unwrap();
        assert_eq!(
            merged,
            vec![
                "public".to_string(),
                "dist/client".to_string(),
                "assets/shared".to_string()
            ]
        );
    }

    #[test]
    fn build_artifact_include_patterns_uses_project_values_when_set() {
        let config = TakoToml {
            build: crate::config::BuildConfig {
                include: vec!["custom/**".to_string()],
                exclude: vec![],
                assets: vec![],
                stages: vec![],
            },
            ..Default::default()
        };
        let includes = build_artifact_include_patterns(&config);
        assert_eq!(includes, vec!["custom/**".to_string()]);
    }

    #[test]
    fn build_artifact_include_patterns_defaults_to_all_when_unset() {
        let includes = build_artifact_include_patterns(&TakoToml::default());
        assert_eq!(includes, vec!["**/*".to_string()]);
    }

    #[test]
    fn should_report_artifact_include_patterns_hides_default_wildcard() {
        assert!(!should_report_artifact_include_patterns(&[
            "**/*".to_string()
        ]));
    }

    #[test]
    fn should_report_artifact_include_patterns_shows_custom_patterns() {
        assert!(should_report_artifact_include_patterns(&[
            "dist/**".to_string()
        ]));
        assert!(should_report_artifact_include_patterns(&[
            "dist/**".to_string(),
            ".output/**".to_string()
        ]));
    }

    #[test]
    fn should_use_docker_build_respects_build_container_flag() {
        let local_preset = BuildPreset {
            name: "bun".to_string(),
            main: None,
            builder_image: None,
            build: crate::build::BuildPresetBuild {
                exclude: vec![],
                install: Some("bun install".to_string()),
                build: Some("bun run build".to_string()),
                targets: vec!["linux-x86_64-glibc".to_string()],
                container: false,
                ..crate::build::BuildPresetBuild::default()
            },
            dev: vec![],
            install: None,
            start: vec![],
            targets: HashMap::new(),
            target_defaults: crate::build::BuildPresetTargetDefaults::default(),
            assets: vec![],
        };
        assert!(!should_use_docker_build(&local_preset));

        let container_preset = BuildPreset {
            build: crate::build::BuildPresetBuild {
                container: true,
                ..local_preset.build.clone()
            },
            ..local_preset
        };
        assert!(should_use_docker_build(&container_preset));
    }

    #[test]
    fn summarize_build_stages_includes_preset_then_custom_stages() {
        let target_build = crate::build::BuildPresetTarget {
            builder_image: None,
            install: Some("bun install".to_string()),
            build: Some("bun run build".to_string()),
        };
        let custom = vec![
            crate::config::BuildStage {
                name: None,
                working_dir: None,
                install: None,
                run: "bun run build".to_string(),
            },
            crate::config::BuildStage {
                name: Some("frontend-assets".to_string()),
                working_dir: Some("frontend".to_string()),
                install: None,
                run: "bun run build".to_string(),
            },
        ];
        assert_eq!(
            summarize_build_stages(&target_build, &custom),
            vec![
                "stage 1 (preset)".to_string(),
                "stage 2".to_string(),
                "stage 3 (frontend-assets)".to_string(),
            ]
        );
    }

    #[test]
    fn run_local_build_executes_preset_then_custom_stages() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let app_dir = workspace.join("apps/web");
        std::fs::create_dir_all(app_dir.join("frontend")).unwrap();
        let target_build = crate::build::BuildPresetTarget {
            builder_image: None,
            install: Some("printf 'preset-install\\n' >> \"$TAKO_APP_DIR/order.log\"".to_string()),
            build: Some("printf 'preset-run\\n' >> \"$TAKO_APP_DIR/order.log\"".to_string()),
        };
        let stages = vec![
            crate::config::BuildStage {
                name: None,
                working_dir: None,
                install: None,
                run: "printf 'stage-2-run\\n' >> \"$TAKO_APP_DIR/order.log\"".to_string(),
            },
            crate::config::BuildStage {
                name: Some("frontend-assets".to_string()),
                working_dir: Some("frontend".to_string()),
                install: Some(
                    "printf 'stage-3-install\\n' >> \"$TAKO_APP_DIR/order.log\"".to_string(),
                ),
                run: "printf 'stage-3-run\\n' >> \"$TAKO_APP_DIR/order.log\"".to_string(),
            },
        ];

        run_local_build(&workspace, "apps/web", &target_build, &stages).unwrap();
        let order = std::fs::read_to_string(app_dir.join("order.log")).unwrap();
        assert_eq!(
            order,
            "preset-install\npreset-run\nstage-2-run\nstage-3-install\nstage-3-run\n"
        );
    }

    #[test]
    fn run_local_build_errors_when_stage_working_dir_is_missing() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let app_dir = workspace.join("apps/web");
        std::fs::create_dir_all(&app_dir).unwrap();
        let target_build = crate::build::BuildPresetTarget {
            builder_image: None,
            install: None,
            build: Some("true".to_string()),
        };
        let stages = vec![crate::config::BuildStage {
            name: None,
            working_dir: Some("frontend".to_string()),
            install: None,
            run: "true".to_string(),
        }];

        let err = run_local_build(&workspace, "apps/web", &target_build, &stages).unwrap_err();
        assert!(err.contains("stage 2"));
        assert!(err.contains("working directory"));
    }

    #[test]
    fn merge_assets_locally_merges_into_public_and_overwrites_last_write() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let app_dir = workspace.join("apps/web");
        std::fs::create_dir_all(app_dir.join("dist/client")).unwrap();
        std::fs::create_dir_all(app_dir.join("assets/shared")).unwrap();
        std::fs::write(app_dir.join("dist/client/logo.txt"), "dist").unwrap();
        std::fs::write(app_dir.join("assets/shared/logo.txt"), "shared").unwrap();

        merge_assets_locally(
            &workspace,
            "apps/web",
            &["dist/client".to_string(), "assets/shared".to_string()],
        )
        .unwrap();

        let merged = std::fs::read_to_string(app_dir.join("public/logo.txt")).unwrap();
        assert_eq!(merged, "shared");
    }

    #[test]
    fn merge_assets_locally_fails_when_asset_root_is_missing() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let app_dir = workspace.join("apps/web");
        std::fs::create_dir_all(&app_dir).unwrap();

        let err =
            merge_assets_locally(&workspace, "apps/web", &["missing".to_string()]).unwrap_err();
        assert!(err.contains("not found after build"));
    }

    #[test]
    fn materialize_runtime_tool_version_writes_prototools_file() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let app_dir = workspace.join("apps/web");
        std::fs::create_dir_all(&app_dir).unwrap();
        materialize_runtime_tool_version(&workspace, "apps/web", "bun", "1.3.9").unwrap();

        let tools_raw = std::fs::read_to_string(app_dir.join(PROTO_TOOLS_FILE)).unwrap();
        let tools = tools_raw.parse::<toml::Table>().unwrap();
        assert_eq!(
            tools.get("bun").and_then(|value| value.as_str()),
            Some("1.3.9")
        );
        assert!(!app_dir.join(RUNTIME_VERSION_OUTPUT_FILE).exists());
    }

    #[test]
    fn materialize_runtime_tool_version_preserves_existing_prototools_entries() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let app_dir = workspace.join("apps/web");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(app_dir.join(PROTO_TOOLS_FILE), "node = \"20.11.1\"\n").unwrap();
        materialize_runtime_tool_version(&workspace, "apps/web", "bun", "1.3.9").unwrap();

        let tools_raw = std::fs::read_to_string(app_dir.join(PROTO_TOOLS_FILE)).unwrap();
        let tools = tools_raw.parse::<toml::Table>().unwrap();
        assert_eq!(
            tools.get("bun").and_then(|value| value.as_str()),
            Some("1.3.9")
        );
        assert_eq!(
            tools.get("node").and_then(|value| value.as_str()),
            Some("20.11.1")
        );
    }

    #[test]
    fn materialize_runtime_tool_version_falls_back_to_workspace_prototools_entries() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let app_dir = workspace.join("apps/web");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(workspace.join(PROTO_TOOLS_FILE), "node = \"20.11.1\"\n").unwrap();

        materialize_runtime_tool_version(&workspace, "apps/web", "bun", "1.3.9").unwrap();

        let tools_raw = std::fs::read_to_string(app_dir.join(PROTO_TOOLS_FILE)).unwrap();
        let tools = tools_raw.parse::<toml::Table>().unwrap();
        assert_eq!(
            tools.get("bun").and_then(|value| value.as_str()),
            Some("1.3.9")
        );
        assert_eq!(
            tools.get("node").and_then(|value| value.as_str()),
            Some("20.11.1")
        );
    }

    #[test]
    fn package_target_artifact_packages_workspace_root_contents() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let app_dir = workspace.join("apps/web");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(workspace.join("README.md"), "repo root").unwrap();
        std::fs::write(app_dir.join("index.ts"), "console.log('ok');").unwrap();
        std::fs::write(app_dir.join("app.json"), r#"{"main":"index.ts"}"#).unwrap();

        let cache_dir = temp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let cache_paths = artifact_cache_paths(&cache_dir, "linux-aarch64-musl", "cache-key");
        let archive_size = package_target_artifact(
            &workspace,
            "apps/web",
            &[],
            &["**/*".to_string()],
            &[],
            &cache_paths,
            "cache-key",
            "index.ts",
            "linux-aarch64-musl",
        )
        .unwrap();
        assert!(archive_size > 0);

        let unpacked = temp.path().join("unpacked");
        BuildExecutor::extract_archive(&cache_paths.artifact_path, &unpacked).unwrap();

        assert!(unpacked.join("README.md").exists());
        assert!(unpacked.join("apps/web/index.ts").exists());
        assert!(unpacked.join("apps/web/app.json").exists());
        assert!(!unpacked.join("index.ts").exists());
    }

    #[test]
    fn package_target_artifact_for_bun_does_not_require_wrapper_sources() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let app_dir = workspace.join("apps/web");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(app_dir.join("index.ts"), "console.log('ok');").unwrap();
        std::fs::write(app_dir.join("app.json"), r#"{"main":"index.ts"}"#).unwrap();

        let cache_dir = temp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let cache_paths = artifact_cache_paths(&cache_dir, "linux-aarch64-musl", "cache-key");
        let archive_size = package_target_artifact(
            &workspace,
            "apps/web",
            &[],
            &["**/*".to_string()],
            &[],
            &cache_paths,
            "cache-key",
            "index.ts",
            "linux-aarch64-musl",
        )
        .unwrap();
        assert!(archive_size > 0);
    }

    #[test]
    fn package_target_artifact_preserves_workspace_protocol_dependencies() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let app_dir = workspace.join("apps/web");
        std::fs::create_dir_all(app_dir.join("src")).unwrap();
        std::fs::write(workspace.join("package.json"), r#"{"private":true}"#).unwrap();
        std::fs::write(
            app_dir.join("package.json"),
            r#"{"name":"web","dependencies":{"tako.sh":"workspace:*"}}"#,
        )
        .unwrap();
        std::fs::write(app_dir.join("src/app.ts"), "export default {};\n").unwrap();

        let cache_dir = temp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let cache_paths = artifact_cache_paths(&cache_dir, "linux-aarch64-musl", "cache-key");
        let archive_size = package_target_artifact(
            &workspace,
            "apps/web",
            &[],
            &["**/*".to_string()],
            &["**/node_modules/**".to_string()],
            &cache_paths,
            "cache-key",
            "src/app.ts",
            "linux-aarch64-musl",
        )
        .unwrap();
        assert!(archive_size > 0);

        let unpacked = temp.path().join("unpacked");
        BuildExecutor::extract_archive(&cache_paths.artifact_path, &unpacked).unwrap();
        let package_json = std::fs::read_to_string(unpacked.join("apps/web/package.json")).unwrap();
        let package_json: serde_json::Value = serde_json::from_str(&package_json).unwrap();
        assert_eq!(
            package_json
                .get("dependencies")
                .and_then(|deps| deps.get("tako.sh"))
                .and_then(|value| value.as_str()),
            Some("workspace:*")
        );
        assert!(!unpacked.join("apps/web/tako_vendor").exists());
    }

    #[test]
    fn package_target_artifact_does_not_validate_workspace_protocol_dependencies() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let app_dir = workspace.join("apps/web");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(
            app_dir.join("package.json"),
            r#"{"name":"web","dependencies":{"missing-pkg":"workspace:*"}}"#,
        )
        .unwrap();
        std::fs::write(app_dir.join("src.ts"), "export default {};\n").unwrap();

        let cache_dir = temp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let cache_paths = artifact_cache_paths(&cache_dir, "linux-aarch64-musl", "cache-key");
        let archive_size = package_target_artifact(
            &workspace,
            "apps/web",
            &[],
            &["**/*".to_string()],
            &[],
            &cache_paths,
            "cache-key",
            "src.ts",
            "linux-aarch64-musl",
        )
        .unwrap();
        assert!(archive_size > 0);
    }

    #[test]
    fn package_target_artifact_fails_when_main_file_is_missing_after_build() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let app_dir = workspace.join("apps/web");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(app_dir.join("index.ts"), "console.log('ok');").unwrap();

        let cache_dir = temp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let cache_paths = artifact_cache_paths(&cache_dir, "linux-aarch64-musl", "cache-key");
        let err = package_target_artifact(
            &workspace,
            "apps/web",
            &[],
            &["**/*".to_string()],
            &[],
            &cache_paths,
            "cache-key",
            "dist/server/tako-entry.mjs",
            "linux-aarch64-musl",
        )
        .unwrap_err();
        assert!(
            err.contains("not found after build"),
            "unexpected error: {err}"
        );
        assert!(
            err.contains("dist/server/tako-entry.mjs"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn validate_main_exists_after_build_rejects_missing_main_file() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let app_dir = workspace.join("apps/web");
        std::fs::create_dir_all(&app_dir).unwrap();

        let err = validate_main_exists_after_build(&workspace, "apps/web", "dist/server/entry.mjs")
            .unwrap_err();
        assert!(
            err.contains("not found after build"),
            "unexpected error: {err}"
        );
        assert!(
            err.contains("dist/server/entry.mjs"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn remote_release_archive_path_uses_artifacts_tar_gz_name() {
        let path = remote_release_archive_path("/opt/tako/apps/my-app/releases/v1");
        assert_eq!(path, "/opt/tako/apps/my-app/releases/v1/artifacts.tar.gz");
    }

    #[test]
    fn build_remote_extract_archive_command_uses_quoted_paths_and_cleanup() {
        let cmd = build_remote_extract_archive_command(
            "/opt/tako/apps/a'b/releases/v1",
            "/opt/tako/apps/a'b/releases/v1/artifacts.tar.gz",
        );
        assert!(cmd.contains("tar -xzf"));
        assert!(cmd.contains(" -C "));
        assert!(cmd.contains("rm -f"));
        assert!(cmd.contains("'\\''"));
    }

    #[test]
    fn build_remote_write_manifest_command_uses_tee() {
        let cmd =
            build_remote_write_manifest_command("/opt/tako/apps/my-app/current/app.json", "e30=");
        assert!(cmd.contains("printf '%s' 'e30=' | base64 -d | tee"));
        assert!(!cmd.contains("echo "));
    }

    #[test]
    fn resolve_deploy_version_uses_source_hash_when_git_commit_missing() {
        let temp = TempDir::new().unwrap();
        let source_root = temp.path().join("source");
        std::fs::create_dir_all(&source_root).unwrap();
        std::fs::write(source_root.join("index.ts"), "export default 1;\n").unwrap();

        let executor = BuildExecutor::new(temp.path());
        let source_hash = executor.compute_source_hash(&source_root).unwrap();
        let (version, _source_hash) =
            resolve_deploy_version_and_source_hash(&executor, &source_root).unwrap();

        assert_eq!(version, format!("nogit_{}", &source_hash[..8]));
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
            "server/index.mjs",
            app_env_vars,
            runtime_env_vars,
            Some(&secrets),
        );

        assert_eq!(manifest.app_name, "my-app");
        assert_eq!(manifest.environment, "staging");
        assert_eq!(manifest.version, "v1");
        assert_eq!(manifest.runtime, "bun");
        assert_eq!(manifest.main, "server/index.mjs");
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
    fn resolve_deploy_main_prefers_tako_toml_main() {
        let config = TakoToml {
            main: Some("server/custom.mjs".to_string()),
            ..Default::default()
        };
        let resolved = resolve_deploy_main(&config, Some("preset-default.ts")).unwrap();
        assert_eq!(resolved, "server/custom.mjs");
    }

    #[test]
    fn resolve_deploy_main_uses_preset_default_main_when_tako_main_is_missing() {
        let resolved =
            resolve_deploy_main(&TakoToml::default(), Some("./dist/server/entry.mjs")).unwrap();
        assert_eq!(resolved, "dist/server/entry.mjs");
    }

    #[test]
    fn resolve_deploy_main_errors_when_tako_and_preset_main_are_missing() {
        let err = resolve_deploy_main(&TakoToml::default(), None).unwrap_err();
        assert!(
            err.contains("Set `main` in tako.toml or preset `main`"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn resolve_deploy_main_rejects_parent_directory_segments_from_tako_toml() {
        let config = TakoToml {
            main: Some("../outside.js".to_string()),
            ..Default::default()
        };
        let err = resolve_deploy_main(&config, None).unwrap_err();
        assert!(
            err.contains("must not contain '..'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn resolve_deploy_main_rejects_empty_tako_toml_main() {
        let config = TakoToml {
            main: Some("  ".to_string()),
            ..Default::default()
        };
        let err = resolve_deploy_main(&config, None).unwrap_err();
        assert!(err.contains("main is empty"), "unexpected error: {err}");
    }

    #[test]
    fn resolve_deploy_main_rejects_invalid_preset_main() {
        let err = resolve_deploy_main(&TakoToml::default(), Some("../outside.js")).unwrap_err();
        assert!(
            err.contains("must not contain '..'"),
            "unexpected error: {err}"
        );
    }
}
