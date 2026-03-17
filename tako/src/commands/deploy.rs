use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::env::current_dir;
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::app::require_app_name_from_config;
use crate::build::{
    BuildAdapter, BuildCache, BuildError, BuildExecutor, BuildPreset, BuildStageCommand,
    PresetGroup, apply_adapter_base_runtime_defaults, compute_file_hash,
    create_filtered_archive_with_prefix, infer_adapter_from_preset_reference, js,
    load_build_preset, qualify_runtime_local_preset_ref, run_container_build,
};
use crate::commands::server;
use crate::config::{BuildStage, SecretsStore, ServerEntry, ServerTarget, ServersToml, TakoToml};
use crate::output;
use crate::ssh::{SshClient, SshConfig, SshError};
use crate::validation::{
    validate_full_config, validate_no_route_conflicts, validate_secrets_for_deployment,
};
use tako_core::{Command, Response};
use tracing::Instrument;

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
    /// SHA-256 hash of the decrypted secrets for this deploy.
    secrets_hash: String,
    app_subdir: String,
    main: String,
    use_unified_target_process: bool,
}

#[derive(Clone)]
struct ServerDeployTarget {
    name: String,
    server: ServerEntry,
    target_label: String,
    archive_path: PathBuf,
    artifact_sha256: String,
}

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct DeployArchiveManifest {
    app_name: String,
    environment: String,
    version: String,
    runtime: String,
    main: String,
    idle_timeout: u32,
    env_vars: BTreeMap<String, String>,
    secret_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    install: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    start: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    commit_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    git_dirty: Option<bool>,
}

struct ValidationResult {
    tako_config: TakoToml,
    servers: ServersToml,
    secrets: SecretsStore,
    env: String,
    warnings: Vec<String>,
}

const ARTIFACT_CACHE_SCHEMA_VERSION: u32 = 4;
const LOCAL_ARTIFACT_CACHE_KEEP_TARGET_ARTIFACTS: usize = 90;
const LOCAL_BUILD_WORKSPACE_RELATIVE_DIR: &str = ".tako/tmp/workspaces";
const RUNTIME_VERSION_OUTPUT_FILE: &str = ".tako-runtime-version";
const UNIFIED_JS_CACHE_TARGET_LABEL: &str = "shared-local-js";

#[derive(Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
struct ArtifactCacheMetadata {
    schema_version: u32,
    artifact_sha256: String,
    artifact_size: u64,
}

#[derive(Debug, Clone)]
struct ArtifactCachePaths {
    artifact_path: PathBuf,
    metadata_path: PathBuf,
}

#[derive(Debug, Clone)]
struct CachedArtifact {
    path: PathBuf,
    size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ArtifactBuildGroup {
    build_target_label: String,
    cache_target_label: String,
    target_labels: Vec<String>,
    display_target_label: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct LocalArtifactCacheCleanupSummary {
    removed_target_artifacts: usize,
    removed_target_metadata: usize,
}

impl LocalArtifactCacheCleanupSummary {
    fn total_removed(self) -> usize {
        self.removed_target_artifacts + self.removed_target_metadata
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
    let source_root = source_bundle_root(&project_dir);

    let validation = output::with_spinner(
        "Validating configuration",
        "Validated",
        || -> Result<ValidationResult, String> {
            let _t = output::timed("Configuration validation");
            let tako_config = TakoToml::load_from_dir(&project_dir).map_err(|e| e.to_string())?;
            let servers = ServersToml::load().map_err(|e| e.to_string())?;
            let secrets = SecretsStore::load_from_dir(&project_dir).map_err(|e| e.to_string())?;

            let env = resolve_deploy_environment(requested_env, &tako_config)?;

            let config_result = validate_full_config(&tako_config, &servers, Some(&env));
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
    )
    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    let ValidationResult {
        tako_config,
        mut servers,
        secrets,
        env,
        warnings,
    } = validation;

    let preflight_preset_ref = resolve_build_preset_ref(&project_dir, &tako_config)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let preflight_runtime_adapter =
        resolve_effective_build_adapter(&project_dir, &tako_config, &preflight_preset_ref)
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    if preflight_runtime_adapter.preset_group() == PresetGroup::Js {
        let _ = js::write_types(&project_dir);
    }

    let _bun_lockfile_checked = if preflight_runtime_adapter == BuildAdapter::Bun {
        output::with_spinner("Checking Bun lockfile", "Bun lockfile valid", || {
            let _t = output::timed("Bun lockfile check");
            run_bun_lockfile_preflight(&source_root)
        })
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?
    } else {
        false
    };

    if requested_env.is_none() {
        output::ContextBlock::new().env(&env).print();
    }

    // Skip confirmation if the user explicitly passed --env production (they
    // already know which environment they're targeting).
    let env_was_explicit = requested_env.is_some();
    confirm_production_deploy(&env, assume_yes || env_was_explicit)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    for warning in &warnings {
        output::warning(&format!("Validation: {}", warning));
    }

    let app_name =
        require_app_name_from_config(&project_dir).map_err(|e| -> Box<dyn std::error::Error> {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()).into()
        })?;
    let routes = required_env_routes(&tako_config, &env)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let server_names =
        resolve_deploy_server_names_with_setup(&tako_config, &mut servers, &env, &project_dir)
            .await
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    // Preflight: check all servers in parallel — verify they're reachable,
    // not upgrading, and (if wildcard routes) have DNS provider configured.
    {
        let _preflight_check_timer = output::timed("Server reachability preflight");
        use crate::commands::server::{apply_dns_config, fetch_dns_config, prompt_dns_setup};

        struct ServerCheck {
            name: String,
            mode: tako_core::UpgradeMode,
            dns_provider: Option<String>,
        }

        let mut check_set = tokio::task::JoinSet::new();
        for server_name in &server_names {
            let server = servers
                .get(server_name)
                .ok_or_else(|| format!("Server '{}' not found in servers.toml", server_name))?;
            let name = server_name.clone();
            let ssh_config = SshConfig::from_server(&server.host, server.port);
            let span = output::scope(&name);
            check_set.spawn(
                async move {
                    tracing::debug!("Preflight check…");
                    let _t = output::timed("Preflight check");
                    let mut ssh = SshClient::new(ssh_config);
                    ssh.connect().await?;
                    let info = ssh.tako_server_info().await?;

                    let mut mode = info.mode;

                    // If server is stuck in upgrading mode, try to clear it.
                    // This handles Ctrl+C / crash during a previous upgrade.
                    if mode == tako_core::UpgradeMode::Upgrading {
                        // Reset the state via SQLite directly (the upgrade lock's
                        // owner may be unknown, and ExitUpgrading requires it).
                        let reset_cmd = SshClient::run_with_root_or_sudo(
                            "sqlite3 /opt/tako/runtime-state.sqlite3 \
                         \"UPDATE server_state SET server_mode = 'normal' WHERE id = 1; \
                          DELETE FROM upgrade_lock WHERE id = 1;\"",
                        );
                        if ssh.exec_checked(&reset_cmd).await.is_ok() {
                            // Restart to pick up the cleared state
                            let _ = ssh.tako_restart().await;
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                            // Re-check
                            if let Ok(new_info) = ssh.tako_server_info().await {
                                mode = new_info.mode;
                            }
                        }
                    }

                    let _ = ssh.disconnect().await;

                    Ok::<_, crate::ssh::SshError>(ServerCheck {
                        name,
                        mode,
                        dns_provider: info.dns_provider,
                    })
                }
                .instrument(span),
            );
        }

        let total = server_names.len();
        let spinner_msg = if total == 1 {
            format!("Checking {}", &server_names[0])
        } else {
            format!("Checking {} servers", total)
        };
        let spinner = output::TrackedSpinner::start(&format!("{spinner_msg}…"));

        let mut checks: Vec<ServerCheck> = Vec::new();
        while let Some(result) = check_set.join_next().await {
            let check = result
                .map_err(|e| -> Box<dyn std::error::Error> { e.to_string().into() })?
                .map_err(|e| -> Box<dyn std::error::Error> { e.to_string().into() })?;

            // Fail fast: server is upgrading
            if check.mode == tako_core::UpgradeMode::Upgrading {
                spinner.finish();
                return Err(format!(
                    "{} is currently upgrading. Retry after the upgrade completes.",
                    check.name,
                )
                .into());
            }

            checks.push(check);
        }

        spinner.finish();

        // Wildcard DNS check
        let wildcard_routes: Vec<_> = routes.iter().filter(|r| r.starts_with("*.")).collect();
        if !wildcard_routes.is_empty() {
            let has_dns: Option<&str> = checks
                .iter()
                .find(|c| c.dns_provider.is_some())
                .map(|c| c.name.as_str());

            let all_have_dns = checks.iter().all(|c| c.dns_provider.is_some());

            if all_have_dns {
                output::success("All servers support wildcard domains");
            } else {
                // Get DNS config: from a server that has it, or prompt once.
                let dns_config = if let Some(donor_name) = has_dns {
                    let donor = servers.get(donor_name).unwrap();
                    let donor_ssh_config = SshConfig::from_server(&donor.host, donor.port);
                    let mut donor_ssh = SshClient::new(donor_ssh_config);
                    donor_ssh
                        .connect()
                        .await
                        .map_err(|e| -> Box<dyn std::error::Error> {
                            format!("Failed to connect to {}: {}", donor_name, e).into()
                        })?;
                    let config = fetch_dns_config(&donor_ssh)
                        .await?
                        .ok_or_else(|| format!("Could not read DNS config from {}", donor_name))?;
                    let _ = donor_ssh.disconnect().await;
                    config
                } else {
                    let wildcard_list = wildcard_routes
                        .iter()
                        .map(|r| format!("`{}`", r))
                        .collect::<Vec<_>>()
                        .join(", ");
                    output::warning(&format!(
                        "Wildcard route {} requires DNS challenge support.",
                        wildcard_list,
                    ));
                    let first = &server_names[0];
                    let first_server = servers.get(first).unwrap();
                    let first_ssh_config =
                        SshConfig::from_server(&first_server.host, first_server.port);
                    let mut first_ssh = SshClient::new(first_ssh_config);
                    first_ssh
                        .connect()
                        .await
                        .map_err(|e| -> Box<dyn std::error::Error> {
                            format!("Failed to connect to {}: {}", first, e).into()
                        })?;
                    let config = prompt_dns_setup(&first_ssh).await?;
                    let _ = first_ssh.disconnect().await;
                    config
                };

                // Apply the same config to all servers to ensure consistency.
                for server_name in &server_names {
                    let server = servers.get(server_name).unwrap();
                    let ssh_config = SshConfig::from_server(&server.host, server.port);
                    let mut ssh = SshClient::new(ssh_config);
                    ssh.connect()
                        .await
                        .map_err(|e| -> Box<dyn std::error::Error> {
                            format!("Failed to connect to {}: {}", server_name, e).into()
                        })?;
                    apply_dns_config(&ssh, server_name, &dns_config).await?;
                    let _ = ssh.disconnect().await;
                }
            }
        } else {
            // No wildcard routes — just report servers are ready.
            if total == 1 {
                output::success(&format!("{} is ready", output::strong(&server_names[0]),));
            } else {
                output::success(&format!("{} servers ready", total));
            }
        }
    }

    let use_per_server_spinners =
        should_use_per_server_spinners(server_names.len(), output::is_interactive());

    let primary_target_and_server = if server_names.len() == 1 {
        let target_name = server_names[0].as_str();
        servers.get(target_name).map(|entry| (target_name, entry))
    } else {
        None
    };
    if output::is_verbose() {
        for line in format_deploy_overview_lines(
            &app_name,
            &env,
            server_names.len(),
            primary_target_and_server,
        ) {
            output::info(&line);
        }
    } else if server_names.len() > 1 {
        output::info(&format!(
            "Deploying to {} servers",
            output::strong(&server_names.len().to_string())
        ));
    }

    // ===== Build =====
    let _phase = output::PhaseSpinner::start("Building…");
    let _build_phase_timer = output::timed("Build phase");

    let executor = BuildExecutor::new(&project_dir);
    let cache = BuildCache::new(project_dir.join(".tako/artifacts"));
    cache
        .init()
        .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
    match cleanup_local_artifact_cache(
        cache.cache_dir(),
        LOCAL_ARTIFACT_CACHE_KEEP_TARGET_ARTIFACTS,
    ) {
        Ok(summary) if summary.total_removed() > 0 => {
            tracing::debug!(
                "Local artifact cache cleanup: removed {} old artifact(s), {} stale metadata file(s)",
                summary.removed_target_artifacts,
                summary.removed_target_metadata
            );
        }
        Ok(_) => {}
        Err(error) => output::warning(&format!("Local artifact cache cleanup skipped: {}", error)),
    }
    let build_workspace_root = project_dir.join(LOCAL_BUILD_WORKSPACE_RELATIVE_DIR);
    match cleanup_local_build_workspaces(&build_workspace_root) {
        Ok(removed) if removed > 0 => {
            tracing::debug!(
                "Local build workspace cleanup: removed {} workspace(s)",
                removed
            );
        }
        Ok(_) => {}
        Err(error) => output::warning(&format!("Local build workspace cleanup skipped: {}", error)),
    }

    let app_subdir = resolve_app_subdir(&source_root, &project_dir)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    tracing::debug!("Source root: {}", source_root.display());
    if !app_subdir.is_empty() {
        tracing::debug!("App directory: {}", app_subdir);
    }

    // Generate version string
    let (version, _source_hash) = resolve_deploy_version_and_source_hash(&executor, &source_root)?;
    let git_commit_message = resolve_git_commit_message(&source_root);
    let git_dirty = executor.is_git_dirty().ok();
    tracing::debug!("Version: {}", version);

    let preset_ref = resolve_build_preset_ref(&project_dir, &tako_config)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    tracing::debug!("Resolving preset ref: {}…", preset_ref);
    let runtime_adapter = resolve_effective_build_adapter(&project_dir, &tako_config, &preset_ref)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let (mut build_preset, resolved_preset) = {
        let _t = output::timed("Preset resolution");
        output::with_spinner_async(
            "Resolving build preset",
            "Build preset resolved",
            load_build_preset(&project_dir, &preset_ref),
        )
        .await
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?
    };
    tracing::debug!(
        "Resolved preset: {} (commit {})",
        resolved_preset.preset_ref,
        shorten_commit(&resolved_preset.commit)
    );
    apply_adapter_base_runtime_defaults(&mut build_preset, runtime_adapter)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    output::bullet(&format!(
        "Build preset: {}",
        output::strong(&resolved_preset.preset_ref)
    ));
    tracing::debug!(
        "Build preset: {} @ {}",
        resolved_preset.preset_ref,
        shorten_commit(&resolved_preset.commit)
    );
    output::bullet(&format_runtime_summary(&build_preset.name, None));
    let runtime_tool = runtime_adapter.id().to_string();

    let manifest_main = resolve_deploy_main(
        &project_dir,
        runtime_adapter,
        &tako_config,
        build_preset.main.as_deref(),
    )
    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    tracing::debug!(
        "{}",
        format_entry_point_summary(&project_dir.join(&manifest_main),)
    );

    let env_idle_timeout = tako_config.get_idle_timeout(&env);
    let manifest = build_deploy_archive_manifest(
        &app_name,
        &env,
        &version,
        runtime_adapter.id(),
        &manifest_main,
        env_idle_timeout,
        build_preset.install.clone(),
        build_preset.start.clone(),
        git_commit_message.clone(),
        git_dirty,
        tako_config.get_merged_vars(&env),
        HashMap::new(),
        secrets.get_env(&env),
    );
    let deploy_secrets = decrypt_deploy_secrets(&app_name, &env, &secrets)?;

    // Create source archive used as input for target-specific builds.
    // The source archive is ephemeral (placed in .tako/tmp/) and also
    // serves as a build lock — its existence means a build is in progress.
    let source_archive_dir = project_dir.join(".tako/tmp");
    std::fs::create_dir_all(&source_archive_dir)
        .map_err(|e| -> Box<dyn std::error::Error> { e.to_string().into() })?;
    let source_archive_path = source_archive_dir.join("source.tar.zst");
    let app_json_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;
    let app_manifest_archive_path = archive_app_manifest_path(&app_subdir);
    let source_archive_size =
        output::with_spinner("Creating source archive", "Source archive created", || {
            tracing::debug!("Archiving source from {}…", source_root.display());
            let _t = output::timed("Source archive creation");
            let size = executor.create_source_archive_with_extra_files(
                &source_root,
                &source_archive_path,
                &[(
                    app_manifest_archive_path.as_str(),
                    app_json_bytes.as_slice(),
                )],
            );
            if let Ok(bytes) = &size {
                tracing::debug!("Source archive size: {}", format_size(*bytes));
            }
            size
        })
        .map_err(|e| -> Box<dyn std::error::Error> { Box::new(e) })?;

    output::bullet(&format_source_archive_created_message());
    tracing::debug!(
        "Source archive created: {} ({})",
        format_path_relative_to(&project_dir, &source_archive_path),
        format_size(source_archive_size),
    );

    let include_patterns = build_artifact_include_patterns(&tako_config);
    let exclude_patterns = build_artifact_exclude_patterns(&build_preset, &tako_config);
    let asset_roots = build_asset_roots(&build_preset, &tako_config)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    // Check all servers exist
    for server_name in &server_names {
        if !servers.contains(server_name) {
            return Err(format_server_not_found_error(server_name).into());
        }
    }
    let server_targets = resolve_deploy_server_targets(&servers, &server_names)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    output::bullet(&format_servers_summary(&server_names));
    let use_unified_js_target_process = should_use_unified_js_target_process(
        &runtime_tool,
        should_use_docker_build(&build_preset),
        &build_preset,
    );
    if let Some(server_targets_summary) =
        format_server_targets_summary(&server_targets, use_unified_js_target_process)
    {
        tracing::debug!("{}", server_targets_summary);
    }

    let artifacts_by_target = build_target_artifacts(
        &project_dir,
        cache.cache_dir(),
        &build_workspace_root,
        &source_archive_path,
        &version,
        &app_subdir,
        &runtime_tool,
        use_unified_js_target_process,
        &manifest_main,
        &server_targets,
        &build_preset,
        &tako_config.build.stages,
        &include_patterns,
        &exclude_patterns,
        &asset_roots,
    )
    .await
    .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    drop(_build_phase_timer);
    _phase.finish("Build complete");

    // ===== Deploy =====
    let _phase = output::PhaseSpinner::start("Deploying…");
    let _deploy_phase_timer = output::timed("Deploy phase");

    let secrets_hash = tako_core::compute_secrets_hash(&deploy_secrets);
    let deployment_app_name = tako_core::deployment_app_id(&app_name, &env);
    let deploy_config = Arc::new(DeployConfig {
        app_name: deployment_app_name.clone(),
        version: version.clone(),
        remote_base: format!("/opt/tako/apps/{}", deployment_app_name),
        routes: routes.clone(),
        env_vars: deploy_secrets,
        secrets_hash,
        app_subdir,
        main: manifest_main,
        use_unified_target_process: use_unified_js_target_process,
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
        let artifact_sha256 = read_artifact_sha256(archive_path)?;
        targets.push(ServerDeployTarget {
            name: server_name.clone(),
            server,
            target_label,
            archive_path: archive_path.clone(),
            artifact_sha256,
        });
    }
    if targets.len() > 1 {
        output::info(&format_parallel_deploy_step(targets.len()));
    }

    // ===== tako-server preflight =====
    let mut missing_servers: Vec<String> = Vec::new();
    let mut not_running_servers: Vec<String> = Vec::new();

    tracing::debug!(
        "Starting tako-server preflight for {} target(s)…",
        targets.len()
    );
    let _preflight_timer = output::timed("tako-server preflight");
    let mut preflight_handles = Vec::new();
    for target in &targets {
        let server_name = target.name.clone();
        let server = target.server.clone();
        let span = output::scope(&server_name);
        preflight_handles.push(tokio::spawn(
            async move {
                let status = check_tako_server(&server).await;
                (server_name, status)
            }
            .instrument(span),
        ));
    }

    let preflight_results = if output::is_interactive() && preflight_handles.len() > 1 {
        output::with_spinner_async_simple(
            &format!(
                "Checking remote servers ({} targets)",
                preflight_handles.len()
            ),
            async {
                let mut results = Vec::new();
                for handle in preflight_handles {
                    results.push(handle.await);
                }
                results
            },
        )
        .await
    } else {
        let mut results = Vec::new();
        for handle in preflight_handles {
            results.push(handle.await);
        }
        results
    };
    drop(_preflight_timer);

    for result in preflight_results {
        let (server_name, status) =
            result.map_err(|e| format!("tako-server preflight task panic: {e}"))?;
        match status {
            Ok(TakoServerStatus::Ready) => {
                tracing::debug!("{server_name} preflight: ready");
            }
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
        let artifact_sha256 = target.artifact_sha256.clone();
        let deploy_config = deploy_config.clone();
        let use_spinner = use_per_server_spinners;
        let span = output::scope(&server_name);
        let handle = tokio::spawn(
            async move {
                let result = deploy_to_server(
                    &deploy_config,
                    &server,
                    &archive_path,
                    &artifact_sha256,
                    &target_label,
                    use_spinner,
                )
                .await;
                (server_name, server, result)
            }
            .instrument(span),
        );
        handles.push(handle);
    }

    // Collect results
    let mut errors = Vec::new();

    let deploy_results =
        if output::is_interactive() && !use_per_server_spinners && handles.len() > 1 {
            output::with_spinner_async_simple(&format_parallel_deploy_step(handles.len()), async {
                let mut results = Vec::new();
                for handle in handles {
                    results.push(handle.await);
                }
                results
            })
            .await
        } else {
            let mut results = Vec::new();
            for handle in handles {
                results.push(handle.await);
            }
            results
        };

    for result in deploy_results {
        match result {
            Ok((server_name, server, result)) => match result {
                Ok(()) => {
                    tracing::debug!("{server_name} deploy succeeded");
                    output::bullet(&format_server_deploy_success(&server_name, &server));
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
    drop(_deploy_phase_timer);
    if errors.is_empty() {
        _phase.finish("Deploy complete");
        if output::is_pretty() {
            eprintln!();
            eprintln!("  {}  {}", output::brand_muted("Revision"), version);
            for (index, route) in routes.iter().enumerate() {
                if index == 0 {
                    eprintln!("  {}       https://{}", output::brand_muted("URL"), route);
                } else {
                    eprintln!("            https://{}", route);
                }
            }
        } else {
            tracing::info!("Revision: {version}");
            for route in &routes {
                tracing::info!("URL: https://{route}");
            }
        }

        Ok(())
    } else {
        let first_err = errors
            .first()
            .map(|s| s.as_str())
            .unwrap_or("unknown error");
        _phase.finish_err("Deploying", first_err);

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
    format!("Deploy to {} now?", output::strong("production"),)
}

fn format_production_deploy_confirm_hint() -> String {
    output::brand_muted("Pass --yes/-y to skip this prompt.")
}

fn confirm_production_deploy(env: &str, assume_yes: bool) -> Result<(), String> {
    if !should_confirm_production_deploy(env, assume_yes, output::is_interactive()) {
        return Ok(());
    }

    output::warning(&format!(
        "You are deploying to {}.",
        output::strong("production")
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
) -> Result<Vec<String>, String> {
    let mut names = super::helpers::resolve_servers_for_env(tako_config, servers, env)?;
    names.sort();
    names.dedup();
    super::helpers::validate_server_names(&names, servers)?;
    Ok(names)
}

async fn resolve_deploy_server_names_with_setup(
    tako_config: &TakoToml,
    servers: &mut ServersToml,
    env: &str,
    project_dir: &Path,
) -> Result<Vec<String>, String> {
    match resolve_deploy_server_names(tako_config, servers, env) {
        Ok(names) => Ok(names),
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
                servers
                    .names()
                    .into_iter()
                    .next()
                    .unwrap_or("<server>")
                    .to_string()
            } else {
                select_production_server_for_mapping(servers)?
            };

            persist_server_env_mapping(project_dir, &selected_server, env)?;
            output::info(&format!(
                "Mapped server {} to {} in tako.toml",
                output::strong(&selected_server),
                output::strong(env)
            ));
            Ok(vec![selected_server])
        }
    }
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
        Some("No servers are configured for production. We will save your selection to tako.toml."),
        options,
    )
    .map_err(|e| format!("Failed to read selection: {}", e))
}

#[cfg(test)]
fn format_prepare_deploy_section(env: &str) -> String {
    format!("Preparing deployment for {}", output::strong(env))
}

fn format_deploy_overview_lines(
    app_name: &str,
    env: &str,
    target_count: usize,
    primary_target_and_server: Option<(&str, &ServerEntry)>,
) -> Vec<String> {
    let mut rows = vec![format!("{:<10}: {}", "App", app_name)];

    match primary_target_and_server {
        Some((target_name, server)) => {
            rows.push(format!("{:<10}: {}", "Target", target_name));
            rows.push(format!(
                "{:<10}: tako@{}:{}",
                "Host", server.host, server.port
            ));
        }
        None => {
            let target_label = if target_count == 1 {
                "1 server".to_string()
            } else {
                format!("{target_count} servers")
            };
            rows.push(format!("{:<10}: {}", "Target", target_label));
        }
    }

    format_box_lines(&format!("Deploy ({})", env), &rows)
}

fn format_box_lines(title: &str, rows: &[String]) -> Vec<String> {
    let top_prefix = format!("─ {} ", title);
    let max_row_width = rows
        .iter()
        .map(|row| row.chars().count())
        .max()
        .unwrap_or(0);
    let inner_width = std::cmp::max(max_row_width + 2, top_prefix.chars().count());
    let row_width = inner_width.saturating_sub(2);

    let mut lines = Vec::with_capacity(rows.len() + 2);
    lines.push(format!(
        "┌{}{}┐",
        top_prefix,
        "─".repeat(inner_width.saturating_sub(top_prefix.chars().count()))
    ));

    for row in rows {
        lines.push(format!("│ {:<row_width$} │", row, row_width = row_width));
    }

    lines.push(format!("└{}┘", "─".repeat(inner_width)));
    lines
}

fn format_build_stages_summary_for_output(
    stage_summary: &[String],
    target_label: Option<&str>,
) -> Option<String> {
    if stage_summary.is_empty() {
        return None;
    }
    Some(format_build_stages_summary(stage_summary, target_label))
}

fn format_build_stages_summary(stage_summary: &[String], target_label: Option<&str>) -> String {
    match target_label {
        Some(label) => format!("Build stages for {}: {}", label, stage_summary.join(" -> ")),
        None => format!("Build stages: {}", stage_summary.join(" -> ")),
    }
}

fn format_runtime_probe_message(target_label: Option<&str>) -> String {
    match target_label {
        Some(label) => format!("Resolving runtime version for {}", label),
        None => "Resolving runtime version".to_string(),
    }
}

fn format_runtime_probe_success(target_label: Option<&str>) -> String {
    match target_label {
        Some(label) => format!("Runtime version resolved for {}", label),
        None => "Runtime version resolved".to_string(),
    }
}

fn format_source_archive_created_message() -> String {
    "Source archive created".to_string()
}

fn format_build_artifact_message(target_label: Option<&str>) -> String {
    match target_label {
        Some(label) => format!("Building artifact for {}", label),
        None => "Building artifact".to_string(),
    }
}

fn format_build_artifact_success(target_label: Option<&str>) -> String {
    match target_label {
        Some(label) => format!("Artifact built for {}", label),
        None => "Artifact built".to_string(),
    }
}

fn format_build_completed_message(target_label: Option<&str>) -> String {
    match target_label {
        Some(label) => format!("Build completed for {}", label),
        None => "Build completed".to_string(),
    }
}

fn format_prepare_artifact_message(target_label: Option<&str>) -> String {
    match target_label {
        Some(label) => format!("Preparing artifact for {}", label),
        None => "Preparing artifact".to_string(),
    }
}

fn format_prepare_artifact_success(target_label: Option<&str>) -> String {
    match target_label {
        Some(label) => format!("Artifact prepared for {}", label),
        None => "Artifact prepared".to_string(),
    }
}

fn format_artifact_cache_hit_message_for_output(target_label: Option<&str>) -> String {
    match target_label {
        Some(label) => format!("Artifact cache hit for {}", label),
        None => "Artifact cache hit".to_string(),
    }
}

fn format_artifact_cache_invalid_message(target_label: Option<&str>, error: &str) -> String {
    match target_label {
        Some(label) => format!(
            "Artifact cache entry for {} is invalid ({}); rebuilding.",
            label, error
        ),
        None => format!("Artifact cache entry is invalid ({}); rebuilding.", error),
    }
}

fn format_artifact_ready_message(
    target_label: Option<&str>,
    artifact_path: &str,
    artifact_size: &str,
) -> String {
    match target_label {
        Some(label) => format!(
            "Artifact ready for {}: {} ({})",
            label, artifact_path, artifact_size
        ),
        None => format!("Artifact ready: {} ({})", artifact_path, artifact_size),
    }
}

fn format_artifact_ready_message_for_output(target_label: Option<&str>) -> String {
    match target_label {
        Some(label) => format!("Artifact ready for {}", label),
        None => "Artifact ready".to_string(),
    }
}

fn format_deploy_main_message(
    main: &str,
    target_label: &str,
    use_unified_target_process: bool,
) -> String {
    if use_unified_target_process {
        return format!("Deploy main: {}", main);
    }
    format!("Deploy main: {} (artifact target: {})", main, target_label)
}

fn format_parallel_deploy_step(server_count: usize) -> String {
    format!("Deploying to {} server(s) in parallel", server_count)
}

fn format_server_deploy_target(name: &str, entry: &ServerEntry) -> String {
    format!("{name} (tako@{}:{})", entry.host, entry.port)
}

fn format_server_deploy_success(name: &str, entry: &ServerEntry) -> String {
    format_server_deploy_target(name, entry)
}

fn format_server_deploy_failure(name: &str, entry: &ServerEntry, error: &str) -> String {
    format!("{}: {}", format_server_deploy_target(name, entry), error)
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
        format!(
            "Failed to update tako.toml with [envs.{env}].servers including '{}': {}",
            server_name, e
        )
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
        "No servers configured for environment '{}'. Add `servers = [\"<name>\"]` under [envs.{}] in tako.toml.",
        env, env
    )
}

fn format_no_global_servers_error() -> String {
    "No servers have been added. Run 'tako servers add <host>' first, then add the server under [envs.production].servers in tako.toml.".to_string()
}

fn format_server_not_found_error(server_name: &str) -> String {
    format!(
        "Server '{}' not found in config.toml [[servers]]. Run 'tako servers add --name {} <host>'.",
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

fn resolve_git_commit_message(source_root: &Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["log", "-1", "--pretty=%s"])
        .current_dir(source_root)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let message = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if message.is_empty() {
        None
    } else {
        Some(message)
    }
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

fn js_entrypoint_extension_for_index_paths(main: &str) -> Option<&str> {
    let extension = if let Some(value) = main.strip_prefix("index.") {
        value
    } else if let Some(value) = main.strip_prefix("src/index.") {
        value
    } else {
        return None;
    };

    if matches!(extension, "ts" | "tsx" | "js" | "jsx") {
        Some(extension)
    } else {
        None
    }
}

fn resolve_js_preset_main_for_project(
    project_dir: &Path,
    runtime_adapter: BuildAdapter,
    preset_main: &str,
) -> Option<String> {
    if !matches!(
        runtime_adapter,
        BuildAdapter::Bun | BuildAdapter::Node | BuildAdapter::Deno
    ) {
        return None;
    }

    let extension = js_entrypoint_extension_for_index_paths(preset_main)?;
    let candidates = [
        format!("index.{extension}"),
        format!("src/index.{extension}"),
    ];
    candidates
        .into_iter()
        .find(|candidate| project_dir.join(candidate).is_file())
}

pub(crate) fn resolve_deploy_main(
    project_dir: &Path,
    runtime_adapter: BuildAdapter,
    tako_config: &TakoToml,
    preset_main: Option<&str>,
) -> Result<String, String> {
    if let Some(main) = &tako_config.main {
        return normalize_main_path(main, "tako.toml");
    }

    if let Some(main) = preset_main {
        let normalized = normalize_main_path(main, "build preset")?;
        if let Some(resolved) =
            resolve_js_preset_main_for_project(project_dir, runtime_adapter, &normalized)
        {
            return Ok(resolved);
        }
        return Ok(normalized);
    }

    Err("No deploy entrypoint configured. Set `main` in tako.toml or preset `main`.".to_string())
}

#[allow(clippy::too_many_arguments)]
fn build_deploy_archive_manifest(
    app_name: &str,
    environment: &str,
    version: &str,
    runtime_name: &str,
    main: &str,
    idle_timeout: u32,
    install: Option<String>,
    start: Vec<String>,
    commit_message: Option<String>,
    git_dirty: Option<bool>,
    app_env_vars: HashMap<String, String>,
    runtime_env_vars: HashMap<String, String>,
    env_secrets: Option<&HashMap<String, String>>,
) -> DeployArchiveManifest {
    let mut secret_names = env_secrets
        .map(|map| map.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    secret_names.sort();

    let mut env_vars =
        build_manifest_env_vars(app_env_vars, runtime_env_vars, environment, runtime_name);
    // TAKO_BUILD is a non-secret env var derived from the version.
    // It's stored in app.json so the server can read it without the CLI.
    env_vars.insert("TAKO_BUILD".to_string(), version.to_string());

    DeployArchiveManifest {
        app_name: app_name.to_string(),
        environment: environment.to_string(),
        version: version.to_string(),
        runtime: runtime_name.to_string(),
        main: main.to_string(),
        idle_timeout,
        env_vars,
        secret_names,
        install,
        start,
        commit_message,
        git_dirty,
    }
}

fn decrypt_deploy_secrets(
    app_name: &str,
    env: &str,
    secrets: &SecretsStore,
) -> Result<HashMap<String, String>, Box<dyn std::error::Error>> {
    let encrypted = match secrets.get_env(env) {
        Some(s) if !s.is_empty() => s,
        _ => return Ok(HashMap::new()),
    };

    let key = super::secret::load_or_derive_key(app_name, env, secrets)?;
    let mut decrypted = HashMap::new();
    for (name, encrypted_value) in encrypted {
        let value = crate::crypto::decrypt(encrypted_value, &key)
            .map_err(|e| format!("Failed to decrypt secret '{}': {}", name, e))?;
        decrypted.insert(name.clone(), value);
    }
    Ok(decrypted)
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

fn format_server_targets_summary(
    server_targets: &[(String, ServerTarget)],
    use_unified_target_process: bool,
) -> Option<String> {
    if use_unified_target_process {
        return None;
    }
    let mut labels = server_targets
        .iter()
        .map(|(_, target)| target.label())
        .collect::<Vec<_>>();
    labels.sort();
    labels.dedup();
    Some(format!("Server targets: {}", labels.join(", ")))
}

fn has_target_specific_build_commands(preset: &BuildPreset) -> bool {
    !preset.targets.is_empty()
}

fn should_use_unified_js_target_process(
    runtime_tool: &str,
    use_docker_build: bool,
    preset: &BuildPreset,
) -> bool {
    !use_docker_build
        && matches!(runtime_tool, "bun" | "node" | "deno")
        && !has_target_specific_build_commands(preset)
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

fn build_artifact_target_groups(
    server_targets: &[(String, ServerTarget)],
    use_unified_target_process: bool,
) -> Vec<ArtifactBuildGroup> {
    let unique_targets: Vec<String> = server_targets
        .iter()
        .map(|(_, target)| target.label())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    if unique_targets.is_empty() {
        return Vec::new();
    }

    if use_unified_target_process {
        let build_target_label = unique_targets
            .first()
            .expect("unique targets cannot be empty")
            .clone();
        return vec![ArtifactBuildGroup {
            build_target_label,
            cache_target_label: UNIFIED_JS_CACHE_TARGET_LABEL.to_string(),
            target_labels: unique_targets,
            display_target_label: None,
        }];
    }

    unique_targets
        .into_iter()
        .map(|label| ArtifactBuildGroup {
            build_target_label: label.clone(),
            cache_target_label: label.clone(),
            target_labels: vec![label.clone()],
            display_target_label: Some(label),
        })
        .collect()
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

fn has_bun_lockfile(workspace_root: &Path) -> bool {
    workspace_root.join("bun.lock").is_file() || workspace_root.join("bun.lockb").is_file()
}

fn run_bun_lockfile_preflight(workspace_root: &Path) -> Result<bool, String> {
    if !has_bun_lockfile(workspace_root) {
        tracing::debug!("No Bun lockfile found, skipping check");
        return Ok(false);
    }

    tracing::debug!("Bun lockfile found, validating frozen lockfile…");
    let output = std::process::Command::new("sh")
        .args(["-lc", "bun install --frozen-lockfile --lockfile-only"])
        .current_dir(workspace_root)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("Failed to run Bun lockfile check: {e}"))?;
    if output.status.success() {
        tracing::debug!("Bun lockfile validation passed");
        return Ok(true);
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let detail = if stderr.is_empty() { stdout } else { stderr };
    Err(format_bun_lockfile_preflight_error(&detail))
}

fn format_bun_lockfile_preflight_error(detail: &str) -> String {
    let normalized = detail.trim();
    if normalized.contains("lockfile had changes, but lockfile is frozen") {
        return "Bun lockfile check failed: package manifests and the Bun lockfile are out of sync. Run `bun install`, commit bun.lock/bun.lockb, then re-run `tako deploy`.".to_string();
    }
    if normalized.is_empty() {
        return "Bun lockfile check failed with no output.".to_string();
    }
    format!("Bun lockfile check failed: {}", normalized)
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
    version: &str,
    target_label: Option<&str>,
) -> ArtifactCachePaths {
    let base = match target_label {
        Some(label) => cache_dir.join(sanitize_cache_label(label)),
        None => cache_dir.to_path_buf(),
    };
    ArtifactCachePaths {
        artifact_path: base.join(format!("{}.tar.zst", version)),
        metadata_path: base.join(format!("{}.json", version)),
    }
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
    keep_target_artifacts: usize,
) -> Result<LocalArtifactCacheCleanupSummary, String> {
    if !cache_dir.exists() {
        return Ok(LocalArtifactCacheCleanupSummary::default());
    }

    let mut summary = LocalArtifactCacheCleanupSummary::default();

    // Collect artifacts from cache_dir itself and from target subdirectories.
    let mut all_artifacts: Vec<(PathBuf, std::time::SystemTime)> = Vec::new();
    let mut all_metadata: Vec<PathBuf> = Vec::new();
    let mut subdirs: Vec<PathBuf> = Vec::new();

    // Scan a single directory, collecting artifacts/metadata and subdirs.
    fn scan_artifact_dir(
        dir: &Path,
        artifacts: &mut Vec<(PathBuf, std::time::SystemTime)>,
        metadata: &mut Vec<PathBuf>,
        mut subdirs: Option<&mut Vec<PathBuf>>,
    ) -> Result<(), String> {
        let entries = match std::fs::read_dir(dir) {
            Ok(entries) => entries,
            Err(_) => return Ok(()),
        };
        for entry in entries {
            let entry =
                entry.map_err(|e| format!("Failed to read dir entry in {}: {e}", dir.display()))?;
            let path = entry.path();
            let file_name = match path.file_name().and_then(|name| name.to_str()) {
                Some(name) => name,
                None => continue,
            };
            let meta = entry
                .metadata()
                .map_err(|e| format!("Failed to read metadata for {}: {e}", path.display()))?;
            if meta.is_dir() {
                if let Some(ref mut subs) = subdirs {
                    subs.push(path);
                }
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            if file_name.ends_with(".tar.zst") {
                artifacts.push((path, meta.modified().unwrap_or(UNIX_EPOCH)));
            } else if file_name.ends_with(".json") {
                metadata.push(path);
            }
        }
        Ok(())
    }

    scan_artifact_dir(
        cache_dir,
        &mut all_artifacts,
        &mut all_metadata,
        Some(&mut subdirs),
    )?;
    for subdir in subdirs {
        scan_artifact_dir(&subdir, &mut all_artifacts, &mut all_metadata, None)?;
    }

    all_artifacts.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| right.0.cmp(&left.0)));

    for (path, _) in all_artifacts.into_iter().skip(keep_target_artifacts) {
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

    for metadata_path in all_metadata {
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
    let stem = file_name.strip_suffix(".tar.zst")?;
    Some(archive_path.with_file_name(format!("{stem}.json")))
}

fn read_artifact_sha256(archive_path: &Path) -> Result<String, String> {
    let metadata_path =
        artifact_cache_metadata_path_for_archive(archive_path).ok_or_else(|| {
            format!(
                "Cannot derive metadata path from {}",
                archive_path.display()
            )
        })?;
    let bytes = std::fs::read(&metadata_path).map_err(|e| {
        format!(
            "Failed to read artifact metadata {}: {e}",
            metadata_path.display()
        )
    })?;
    let metadata: ArtifactCacheMetadata = serde_json::from_slice(&bytes).map_err(|e| {
        format!(
            "Failed to parse artifact metadata {}: {e}",
            metadata_path.display()
        )
    })?;
    Ok(metadata.artifact_sha256)
}

fn artifact_cache_archive_path_for_metadata(metadata_path: &Path) -> Option<PathBuf> {
    let file_name = metadata_path.file_name()?.to_str()?;
    let stem = file_name.strip_suffix(".json")?;
    Some(metadata_path.with_file_name(format!("{stem}.tar.zst")))
}

fn remove_cached_artifact_files(paths: &ArtifactCachePaths) {
    let _ = std::fs::remove_file(&paths.artifact_path);
    let _ = std::fs::remove_file(&paths.metadata_path);
}

fn load_valid_cached_artifact(
    paths: &ArtifactCachePaths,
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

#[allow(clippy::too_many_arguments)]
async fn build_target_artifacts(
    project_dir: &Path,
    cache_dir: &Path,
    build_workspace_root: &Path,
    source_archive_path: &Path,
    version: &str,
    app_subdir: &str,
    runtime_tool: &str,
    use_unified_target_process: bool,
    main: &str,
    server_targets: &[(String, ServerTarget)],
    preset: &BuildPreset,
    custom_stages: &[BuildStage],
    include_patterns: &[String],
    exclude_patterns: &[String],
    asset_roots: &[String],
) -> Result<HashMap<String, PathBuf>, String> {
    let target_groups = build_artifact_target_groups(server_targets, use_unified_target_process);
    let has_multiple_targets = target_groups.len() > 1;
    let use_docker_build = should_use_docker_build(preset);
    let mut artifacts = HashMap::new();

    for target_group in target_groups {
        let build_target_label = target_group.build_target_label;
        let cache_target_label = target_group.cache_target_label;
        let display_target_label = target_group.display_target_label.as_deref();
        let target_build = resolve_build_target(preset, &build_target_label)?;
        let use_local_build_spinners = should_use_local_build_spinners(output::is_interactive());
        let stage_summary = summarize_build_stages(&target_build, custom_stages);
        if let Some(stage_summary_message) =
            format_build_stages_summary_for_output(&stage_summary, display_target_label)
        {
            tracing::debug!("{}", stage_summary_message);
        }
        std::fs::create_dir_all(build_workspace_root)
            .map_err(|e| format!("Failed to create {}: {e}", build_workspace_root.display()))?;
        let workspace =
            build_workspace_root.join(format!("work-{}-{}", version, build_target_label));
        if workspace.exists() {
            std::fs::remove_dir_all(&workspace)
                .map_err(|e| format!("Failed to clear {}: {e}", workspace.display()))?;
        }
        std::fs::create_dir_all(&workspace)
            .map_err(|e| format!("Failed to create {}: {e}", workspace.display()))?;
        BuildExecutor::extract_archive(source_archive_path, &workspace).map_err(|e| {
            match display_target_label {
                Some(label) => format!("Failed to extract source archive for {}: {}", label, e),
                None => format!("Failed to extract source archive: {}", e),
            }
        })?;

        let runtime_probe_label = format_runtime_probe_message(display_target_label);
        let runtime_probe_success = format_runtime_probe_success(display_target_label);
        let runtime_version = if use_local_build_spinners {
            output::with_spinner(&runtime_probe_label, &runtime_probe_success, || {
                tracing::debug!(
                    "Probing {} version in {}…",
                    runtime_tool,
                    workspace.display()
                );
                let _t = output::timed("Runtime probe");
                let version = if use_docker_build {
                    resolve_runtime_version_with_docker_probe(
                        &workspace,
                        app_subdir,
                        &build_target_label,
                        runtime_tool,
                        &target_build,
                    )
                } else {
                    resolve_runtime_version_from_workspace(&workspace, app_subdir, runtime_tool)
                };
                if let Ok(v) = &version {
                    tracing::debug!("Detected {} {}", runtime_tool, v);
                }
                version
            })?
        } else {
            tracing::debug!("{}", runtime_probe_label);
            let _t = output::timed("Runtime probe");
            let version = if use_docker_build {
                resolve_runtime_version_with_docker_probe(
                    &workspace,
                    app_subdir,
                    &build_target_label,
                    runtime_tool,
                    &target_build,
                )?
            } else {
                resolve_runtime_version_from_workspace(&workspace, app_subdir, runtime_tool)?
            };
            drop(_t);
            tracing::debug!("Detected {} {}", runtime_tool, version);
            version
        };

        let target_label_for_path = if has_multiple_targets {
            Some(cache_target_label.as_str())
        } else {
            None
        };
        let cache_paths = artifact_cache_paths(cache_dir, version, target_label_for_path);

        match load_valid_cached_artifact(&cache_paths) {
            Ok(Some(cached)) => {
                tracing::debug!(
                    "Artifact cache hit: {} ({})",
                    format_path_relative_to(project_dir, &cached.path),
                    format_size(cached.size_bytes)
                );
                output::bullet(&format_artifact_cache_hit_message_for_output(
                    display_target_label,
                ));
                for target_label in &target_group.target_labels {
                    artifacts.insert(target_label.clone(), cached.path.clone());
                }
                let _ = std::fs::remove_dir_all(&workspace);
                continue;
            }
            Ok(None) => {
                tracing::debug!("Artifact cache miss, building from source");
            }
            Err(error) => {
                output::warning(&format_artifact_cache_invalid_message(
                    display_target_label,
                    &error,
                ));
                remove_cached_artifact_files(&cache_paths);
            }
        }

        let build_result = (|| -> Result<u64, String> {
            let build_label = format_build_artifact_message(display_target_label);
            let build_success = format_build_artifact_success(display_target_label);
            if use_local_build_spinners {
                output::with_spinner(&build_label, &build_success, || {
                    tracing::debug!(
                        "Building target {} (docker={})…",
                        build_target_label,
                        use_docker_build
                    );
                    let _t = output::timed("Target build");
                    run_target_build(
                        &workspace,
                        app_subdir,
                        &build_target_label,
                        runtime_tool,
                        use_docker_build,
                        &target_build,
                        custom_stages,
                    )
                })?;
            } else {
                output::bullet(&build_label);
                let _t = output::timed("Target build");
                run_target_build(
                    &workspace,
                    app_subdir,
                    &build_target_label,
                    runtime_tool,
                    use_docker_build,
                    &target_build,
                    custom_stages,
                )?;
            }
            save_runtime_version_to_manifest(&workspace, app_subdir, &runtime_version)?;
            output::bullet(&format_build_completed_message(display_target_label));

            let prepare_label = format_prepare_artifact_message(display_target_label);
            let prepare_success = format_prepare_artifact_success(display_target_label);
            if use_local_build_spinners {
                output::with_spinner(&prepare_label, &prepare_success, || {
                    tracing::debug!("Packaging artifact for {}…", build_target_label);
                    let _t = output::timed("Artifact packaging");
                    package_target_artifact(
                        &workspace,
                        app_subdir,
                        asset_roots,
                        include_patterns,
                        exclude_patterns,
                        &cache_paths,
                        main,
                        &build_target_label,
                    )
                })
            } else {
                output::bullet(&prepare_label);
                tracing::debug!("Packaging artifact for {}…", build_target_label);
                let _t = output::timed("Artifact packaging");
                package_target_artifact(
                    &workspace,
                    app_subdir,
                    asset_roots,
                    include_patterns,
                    exclude_patterns,
                    &cache_paths,
                    main,
                    &build_target_label,
                )
            }
        })();
        let _ = std::fs::remove_dir_all(&workspace);
        let artifact_size = build_result?;

        output::bullet(&format_artifact_ready_message_for_output(
            display_target_label,
        ));
        tracing::debug!(
            "{}",
            format_artifact_ready_message(
                display_target_label,
                &format_path_relative_to(project_dir, &cache_paths.artifact_path),
                &format_size(artifact_size),
            )
        );
        for target_label in &target_group.target_labels {
            artifacts.insert(target_label.clone(), cache_paths.artifact_path.clone());
        }
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
        run_local_build(
            workspace,
            app_subdir,
            runtime_tool,
            target_build,
            custom_stages,
        )?;
    }
    Ok(())
}

fn run_local_build(
    workspace: &Path,
    app_subdir: &str,
    _runtime_tool: &str,
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
                .stdin(std::process::Stdio::null())
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
        tracing::debug!("Running {preset_stage_label} install: {install}…");
        let _t = output::timed(&format!("{preset_stage_label} install"));
        run_shell(workspace, install, "install", preset_stage_label)?;
    }
    if let Some(build) = target_build
        .build
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        tracing::debug!("Running {preset_stage_label} build: {build}…");
        let _t = output::timed(&format!("{preset_stage_label} build"));
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
            tracing::debug!("Running {stage_label} install: {install}…");
            let _t = output::timed(&format!("{stage_label} install"));
            run_shell(&stage_cwd, install, "install", &stage_label)?;
        }
        let run_command = stage.run.trim();
        if run_command.is_empty() {
            return Err(format!("Local {stage_label} run command is empty"));
        }
        tracing::debug!("Running {stage_label}: {run_command}…");
        let _t = output::timed(&stage_label);
        run_shell(&stage_cwd, run_command, "run", &stage_label)?;
        drop(_t);
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

/// Save the resolved runtime version into the deploy manifest (`app.json`).
fn save_runtime_version_to_manifest(
    workspace: &Path,
    app_subdir: &str,
    runtime_version: &str,
) -> Result<(), String> {
    let app_dir = workspace_app_dir(workspace, app_subdir);
    let manifest_path = app_dir.join("app.json");
    let content = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("Failed to read {}: {e}", manifest_path.display()))?;
    let mut value: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse {}: {e}", manifest_path.display()))?;
    value["runtime_version"] = serde_json::Value::String(runtime_version.to_string());
    let updated = serde_json::to_string_pretty(&value)
        .map_err(|e| format!("Failed to serialize {}: {e}", manifest_path.display()))?;
    std::fs::write(&manifest_path, updated)
        .map_err(|e| format!("Failed to write {}: {e}", manifest_path.display()))?;
    let _ = std::fs::remove_file(app_dir.join(RUNTIME_VERSION_OUTPUT_FILE));
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

    #[cfg(test)]
    {
        let _ = (workspace, &app_dir, runtime_tool);
        Ok("latest".to_string())
    }

    #[cfg(not(test))]
    {
        let command = format!("{} --version", shell_single_quote(runtime_tool));
        let output = std::process::Command::new("sh")
            .args(["-lc", &command])
            .current_dir(&app_dir)
            .stdin(std::process::Stdio::null())
            .output();
        match output {
            Ok(out) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                if let Some(version) = stdout
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .map(str::to_string)
                {
                    return Ok(version);
                }
                Ok("latest".to_string())
            }
            _ => Ok("latest".to_string()),
        }
    }
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

#[allow(clippy::too_many_arguments)]
fn package_target_artifact(
    workspace: &Path,
    app_subdir: &str,
    asset_roots: &[String],
    include_patterns: &[String],
    exclude_patterns: &[String],
    cache_paths: &ArtifactCachePaths,
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
    tracing::debug!(
        "Artifact size for {}: {}",
        target_label,
        format_size(artifact_size)
    );

    if let Err(error) = persist_cached_artifact(&artifact_temp_path, cache_paths, artifact_size) {
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
    serde_json::from_str::<serde_json::Value>(response)
        .ok()
        .and_then(|value| {
            value
                .get("status")
                .and_then(|status| status.as_str())
                .map(|status| status == "error")
        })
        .unwrap_or(false)
}

const DEPLOY_DISK_CHECK_PATH: &str = "/opt/tako";
const DEPLOY_DISK_SPACE_MULTIPLIER: u64 = 3;
const DEPLOY_DISK_SPACE_HEADROOM_BYTES: u64 = 64 * 1024 * 1024;

use crate::shell::shell_single_quote;

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
    loading: &str,
    success: &str,
    use_spinner: bool,
    work: Fut,
) -> Result<T, Box<dyn std::error::Error + Send + Sync>>
where
    Fut: Future<Output = Result<T, E>> + Send,
    T: Send,
    E: Send + std::fmt::Display + Into<Box<dyn std::error::Error + Send + Sync>>,
{
    if use_spinner {
        output::with_spinner_async(loading, success, work)
            .await
            .map_err(Into::into)
    } else {
        tracing::debug!("{}", loading);
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
    format!("{release_dir}/artifacts.tar.zst")
}

fn build_remote_extract_archive_command(release_dir: &str, remote_archive: &str) -> String {
    format!(
        "tako-server --extract-zstd-archive {} --extract-dest {} && rm -f {}",
        shell_single_quote(remote_archive),
        shell_single_quote(release_dir),
        shell_single_quote(remote_archive)
    )
}

/// Deploy to a single server
async fn deploy_to_server(
    config: &DeployConfig,
    server: &ServerEntry,
    archive_path: &Path,
    artifact_sha256: &str,
    target_label: &str,
    use_spinner: bool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing::debug!("Deploying (target: {target_label}, port: {})…", server.port);
    let _server_deploy_timer = output::timed("Server deploy");
    let ssh_config = SshConfig::from_server(&server.host, server.port);
    let mut ssh = SshClient::new(ssh_config);
    run_deploy_step("Connecting", "Connected", use_spinner, ssh.connect()).await?;
    let archive_size_bytes = std::fs::metadata(archive_path)?.len();
    tracing::debug!("Archive size: {}", format_size(archive_size_bytes));

    run_deploy_step(
        "Checking remote disk space",
        "Disk space OK",
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
            run_deploy_step("Checking tako-server", "tako-server found", use_spinner, ssh.is_tako_installed())
                .await?;
        if !installed {
            return Err(
                "tako-server is not installed on this server. Install it as root (see scripts/install-tako-server.sh)."
                    .into(),
            );
        }

        // Ensure the service is running before socket commands.
        run_deploy_step(
            "Checking tako-server status",
            "tako-server running",
            use_spinner,
            ensure_tako_running(&mut ssh),
        )
        .await?;

        // Route conflict validation (best-effort against current tako-server state)
        let existing = run_deploy_step("Checking route conflicts", "No route conflicts", use_spinner, async {
            parse_existing_routes_response(ssh.tako_routes().await?)
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })
        })
        .await?;

        validate_no_route_conflicts(&existing, &config.app_name, &config.routes)
            .map_err(|e| format!("Route conflict: {}", e))?;

        // Create directories.
        run_deploy_step("Creating directories", "Directories created", use_spinner, async {
            ssh.exec_checked(&format!(
                "mkdir -p {} {} {}",
                release_dir, release_app_dir, config.shared_dir()
            )).await?;
            Ok::<(), SshError>(())
        })
        .await?;

        // Upload target-specific archive artifact (skip if server already has it).
        let remote_archive = remote_release_archive_path(&release_dir);
        let artifact_cache_dir = "/opt/tako/artifact-cache";
        let cached_artifact_path = format!("{}/{}.tar.zst", artifact_cache_dir, artifact_sha256);
        let has_cached = ssh
            .exec(&format!(
                "test -f {} && echo hit || echo miss",
                cached_artifact_path
            ))
            .await
            .map(|r| r.stdout.trim() == "hit")
            .unwrap_or(false);

        if has_cached {
            tracing::debug!("Remote artifact cache hit, skipping upload");
            run_deploy_step(
                "Linking cached artifact",
                "Artifact cached (skip upload)",
                use_spinner,
                async {
                    ssh.exec_checked(&format!(
                        "cp {} {}",
                        cached_artifact_path, remote_archive
                    ))
                    .await?;
                    Ok::<(), SshError>(())
                },
            )
            .await?;
        } else {
            tracing::debug!("Uploading artifact ({})…", format_size(archive_size_bytes));
            let upload_timer = output::timed("Artifact upload");
            if use_spinner {
                let tp = std::sync::Arc::new(output::TransferProgress::new(
                    "Uploading",
                    "Upload complete",
                    archive_size_bytes,
                ));
                let tp2 = tp.clone();
                ssh.upload_with_progress(
                    archive_path,
                    &remote_archive,
                    Some(Box::new(move |done, _total| tp2.set_position(done))),
                )
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
                tp.finish();
            } else {
                ssh.upload(archive_path, &remote_archive)
                    .await
                    .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
            }
            drop(upload_timer);
            // Cache the artifact for future deploys (best-effort).
            let _ = ssh
                .exec(&format!(
                    "mkdir -p {} && cp {} {}",
                    artifact_cache_dir, remote_archive, cached_artifact_path
                ))
                .await;
        }

        // Extract archive, symlink shared dirs, and write runtime manifest in one exec.
        tracing::debug!("Extracting and configuring release…");
        let extract_timer = output::timed("Release extraction");
        run_deploy_step("Extracting and configuring release", "Release configured", use_spinner, async {
            let extract_cmd = build_remote_extract_archive_command(&release_dir, &remote_archive);
            let shared_link_cmd = format!(
                "mkdir -p {}/logs && ln -sfn {}/logs {}/logs",
                config.shared_dir(),
                config.shared_dir(),
                release_dir
            );
            let combined_cmd = format!("{} && {}", extract_cmd, shared_link_cmd);
            ssh.exec_checked(&combined_cmd).await?;
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        })
        .await?;
        drop(extract_timer);
        tracing::debug!("{}", format_deploy_main_message(
            &config.main,
            target_label,
            config.use_unified_target_process,
        ));

        // Check if server already has up-to-date secrets by comparing hashes.
        // If hashes match, skip sending secrets (server keeps existing ones).
        let deploy_secrets = match query_remote_secrets_hash(&ssh, &config.app_name).await {
            Some(remote_hash) if remote_hash == config.secrets_hash => None,
            _ => Some(config.env_vars.clone()),
        };

        // Send deploy command to tako-server.
        let cmd = Command::Deploy {
            app: config.app_name.clone(),
            version: config.version.clone(),
            path: release_app_dir.clone(),
            routes: config.routes.clone(),
            secrets: deploy_secrets,
        };
        let json = serde_json::to_string(&cmd)?;
        let response =
            run_deploy_step("Notifying tako-server", "tako-server notified", use_spinner, ssh.tako_command(&json))
                .await?;

        // Parse response.
        if deploy_response_has_error(&response) {
            return Err(format!("tako-server error: {}", response).into());
        }

        // Update current symlink only after tako-server accepted the deploy command.
        run_deploy_step(
            "Updating current symlink",
            "Current symlink updated",
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
            "Cleaning old releases",
            "Old releases cleaned",
            use_spinner,
            ssh.exec(&cleanup_cmd),
        )
        .await?;

        Ok(())
    }
    .await;

    if result.is_err()
        && !release_dir_preexisted
        && let Err(e) = cleanup_partial_release(&ssh, &release_dir).await
    {
        tracing::warn!("Failed to cleanup partial release directory {release_dir}: {e}");
    }

    // Always disconnect (best-effort).
    let _ = ssh.disconnect().await;

    result
}

/// Query the remote server for the SHA-256 hash of an app's current secrets.
/// Returns `None` if the query fails.
async fn query_remote_secrets_hash(ssh: &SshClient, app_name: &str) -> Option<String> {
    let cmd = Command::GetSecretsHash {
        app: app_name.to_string(),
    };
    let json = serde_json::to_string(&cmd).ok()?;
    let response_str = ssh.tako_command(&json).await.ok()?;
    let value: serde_json::Value = serde_json::from_str(&response_str).ok()?;
    if value.get("status").and_then(|s| s.as_str()) != Some("ok") {
        return None;
    }
    value
        .get("data")
        .and_then(|d| d.get("hash"))
        .and_then(|h| h.as_str())
        .map(|s| s.to_string())
}

async fn check_tako_server(
    server: &ServerEntry,
) -> Result<TakoServerStatus, Box<dyn std::error::Error + Send + Sync>> {
    let mut ssh = SshClient::connect_to(&server.host, server.port).await?;

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

    // Non-systemd/non-OpenRC hosts or restrictive sudo policies can report "unknown".
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

    Err(format!(
        "tako-server is installed but not running. Start the service (e.g. `{}`), then retry.",
        crate::ssh::SshClient::tako_start_hint()
    )
    .into())
}

/// Alias for the shared size formatter.
fn format_size(bytes: u64) -> String {
    output::format_size(bytes)
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
    use crate::config::{EnvConfig, ServerEntry, ServersToml, TakoToml};
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
            runtime: Some("bun".to_string()),
            preset: Some("tanstack-start@abc1234".to_string()),
            ..Default::default()
        };

        assert_eq!(
            resolve_build_preset_ref(temp.path(), &config).unwrap(),
            "js/tanstack-start@abc1234"
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
            "js/tanstack-start"
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
    fn resolve_effective_build_adapter_uses_preset_group_when_detection_is_unknown() {
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
    fn has_bun_lockfile_detects_both_supported_lockfiles() {
        let temp = TempDir::new().unwrap();
        assert!(!has_bun_lockfile(temp.path()));

        std::fs::write(temp.path().join("bun.lock"), "").unwrap();
        assert!(has_bun_lockfile(temp.path()));

        std::fs::remove_file(temp.path().join("bun.lock")).unwrap();
        std::fs::write(temp.path().join("bun.lockb"), "").unwrap();
        assert!(has_bun_lockfile(temp.path()));
    }

    #[test]
    fn run_bun_lockfile_preflight_skips_when_lockfile_is_missing() {
        let temp = TempDir::new().unwrap();
        let checked = run_bun_lockfile_preflight(temp.path()).unwrap();
        assert!(!checked);
    }

    #[test]
    fn format_bun_lockfile_preflight_error_includes_fix_hint_for_frozen_lockfile_mismatch() {
        let message = format_bun_lockfile_preflight_error(
            "error: lockfile had changes, but lockfile is frozen",
        );
        assert!(message.contains("Bun lockfile check failed"));
        assert!(message.contains("Run `bun install`"));
        assert!(message.contains("bun.lock"));
    }

    #[test]
    fn format_bun_lockfile_preflight_error_falls_back_to_raw_detail() {
        let message = format_bun_lockfile_preflight_error("permission denied");
        assert_eq!(message, "Bun lockfile check failed: permission denied");
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
        config.envs.insert(
            "production".to_string(),
            EnvConfig {
                servers: vec!["prod-1".to_string()],
                ..Default::default()
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

        let resolved = resolve_deploy_server_names(&config, &servers, "production").unwrap();
        assert_eq!(resolved, vec!["prod-1".to_string()]);
    }

    #[test]
    fn resolve_deploy_servers_require_explicit_mapping() {
        let mut config = TakoToml::default();
        config
            .envs
            .insert("production".to_string(), Default::default());

        let mut servers = ServersToml::default();
        servers.servers.insert(
            "solo".to_string(),
            ServerEntry {
                host: "127.0.0.1".to_string(),
                port: 22,
                description: None,
            },
        );

        let err = resolve_deploy_server_names(&config, &servers, "production").unwrap_err();
        assert!(err.contains("No servers configured for environment 'production'"));
    }

    #[test]
    fn resolve_deploy_servers_errors_with_hint_when_no_global_servers_exist() {
        let mut config = TakoToml::default();
        config
            .envs
            .insert("production".to_string(), Default::default());
        let servers = ServersToml::default();

        let err = resolve_deploy_server_names(&config, &servers, "production").unwrap_err();
        assert!(err.contains("No servers have been added"));
        assert!(err.contains("tako servers add <host>"));
    }

    #[test]
    fn resolve_deploy_servers_errors_for_non_production_without_mapping() {
        let mut config = TakoToml::default();
        config
            .envs
            .insert("staging".to_string(), Default::default());
        let mut servers = ServersToml::default();
        servers.servers.insert(
            "solo".to_string(),
            ServerEntry {
                host: "127.0.0.1".to_string(),
                port: 22,
                description: None,
            },
        );

        let err = resolve_deploy_server_names(&config, &servers, "staging").unwrap_err();
        assert!(err.contains("No servers configured for environment 'staging'"));
    }

    #[test]
    fn persist_server_env_mapping_updates_env_server_list() {
        let temp_dir = TempDir::new().unwrap();
        std::fs::write(
            temp_dir.path().join("tako.toml"),
            r#"
name = "test-app"

[envs.production]
route = "app.example.com"
"#,
        )
        .unwrap();

        persist_server_env_mapping(temp_dir.path(), "tako-server", "production").unwrap();

        let saved = TakoToml::load_from_dir(temp_dir.path()).unwrap();
        assert_eq!(saved.get_servers_for_env("production"), vec!["tako-server"]);
    }

    #[tokio::test]
    async fn resolve_deploy_servers_with_setup_persists_single_server_mapping() {
        let config = TakoToml {
            name: Some("test-app".to_string()),
            envs: [(
                "production".to_string(),
                EnvConfig {
                    route: Some("app.example.com".to_string()),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
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

[envs.production]
route = "app.example.com"
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
            secrets_hash: String::new(),
            app_subdir: "examples/bun".to_string(),
            main: "index.ts".to_string(),
            use_unified_target_process: false,
        };
        assert_eq!(cfg.release_dir(), "/opt/tako/apps/my-app/releases/v1");
        assert_eq!(
            cfg.release_app_dir(),
            "/opt/tako/apps/my-app/releases/v1/examples/bun"
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
        let summary = format_server_targets_summary(
            &[
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
            ],
            false,
        );

        assert_eq!(
            summary,
            Some("Server targets: linux-aarch64-musl, linux-x86_64-glibc".to_string())
        );
    }

    #[test]
    fn format_server_targets_summary_hides_line_for_unified_mode() {
        let summary = format_server_targets_summary(
            &[(
                "a".to_string(),
                ServerTarget {
                    arch: "aarch64".to_string(),
                    libc: "musl".to_string(),
                },
            )],
            true,
        );

        assert_eq!(summary, None);
    }

    #[test]
    fn should_use_unified_js_target_process_only_for_local_non_overridden_js_builds() {
        let mut preset = BuildPreset {
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

        assert!(should_use_unified_js_target_process("bun", false, &preset));
        assert!(should_use_unified_js_target_process("node", false, &preset));
        assert!(should_use_unified_js_target_process("deno", false, &preset));
        assert!(!should_use_unified_js_target_process("go", false, &preset));
        assert!(!should_use_unified_js_target_process("bun", true, &preset));

        preset.targets.insert(
            "linux-x86_64-glibc".to_string(),
            crate::build::BuildPresetTarget {
                builder_image: None,
                install: Some("bun install".to_string()),
                build: Some("bun run build".to_string()),
            },
        );
        assert!(!should_use_unified_js_target_process("bun", false, &preset));
    }

    #[test]
    fn build_artifact_target_groups_unifies_targets_when_requested() {
        let server_targets = vec![
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
                    arch: "aarch64".to_string(),
                    libc: "musl".to_string(),
                },
            ),
        ];

        let groups = build_artifact_target_groups(&server_targets, true);
        assert_eq!(
            groups,
            vec![ArtifactBuildGroup {
                build_target_label: "linux-aarch64-musl".to_string(),
                cache_target_label: UNIFIED_JS_CACHE_TARGET_LABEL.to_string(),
                target_labels: vec![
                    "linux-aarch64-musl".to_string(),
                    "linux-x86_64-glibc".to_string()
                ],
                display_target_label: None,
            }]
        );
    }

    #[test]
    fn build_artifact_target_groups_keeps_per_target_groups_when_not_unified() {
        let server_targets = vec![
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
                    arch: "aarch64".to_string(),
                    libc: "musl".to_string(),
                },
            ),
        ];

        let groups = build_artifact_target_groups(&server_targets, false);
        assert_eq!(
            groups,
            vec![
                ArtifactBuildGroup {
                    build_target_label: "linux-aarch64-musl".to_string(),
                    cache_target_label: "linux-aarch64-musl".to_string(),
                    target_labels: vec!["linux-aarch64-musl".to_string()],
                    display_target_label: Some("linux-aarch64-musl".to_string()),
                },
                ArtifactBuildGroup {
                    build_target_label: "linux-x86_64-glibc".to_string(),
                    cache_target_label: "linux-x86_64-glibc".to_string(),
                    target_labels: vec!["linux-x86_64-glibc".to_string()],
                    display_target_label: Some("linux-x86_64-glibc".to_string()),
                },
            ]
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
            "prod (tako@example.com:2222)"
        );
        assert_eq!(
            format_server_deploy_failure("prod", &server, "boom"),
            "prod (tako@example.com:2222): boom"
        );
    }

    #[test]
    fn deploy_overview_lines_include_primary_target_host_when_single_server() {
        let server = ServerEntry {
            host: "localhost".to_string(),
            port: 2222,
            description: None,
        };
        let lines =
            format_deploy_overview_lines("bun", "production", 1, Some(("testbed", &server)));
        assert_eq!(lines.len(), 5);
        assert!(lines[0].starts_with("┌─ Deploy (production)"));
        assert!(lines[1].contains("App"));
        assert!(lines[1].contains("bun"));
        assert!(lines[2].contains("Target"));
        assert!(lines[2].contains("testbed"));
        assert!(lines[3].contains("Host"));
        assert!(lines[3].contains("tako@localhost:2222"));
        assert!(lines[4].starts_with("└"));
    }

    #[test]
    fn deploy_overview_lines_include_server_count_for_multi_target() {
        let lines = format_deploy_overview_lines("bun", "staging", 3, None);
        assert_eq!(lines.len(), 4);
        assert!(lines[0].starts_with("┌─ Deploy (staging)"));
        assert!(lines[1].contains("App"));
        assert!(lines[1].contains("bun"));
        assert!(lines[2].contains("Target"));
        assert!(lines[2].contains("3 servers"));
        assert!(lines[3].starts_with("└"));
    }

    #[test]
    fn format_deploy_main_message_omits_target_for_unified_process() {
        assert_eq!(
            format_deploy_main_message("dist/server/tako-entry.mjs", "linux-aarch64-musl", true),
            "Deploy main: dist/server/tako-entry.mjs"
        );
        assert_eq!(
            format_deploy_main_message("dist/server/tako-entry.mjs", "linux-aarch64-musl", false),
            "Deploy main: dist/server/tako-entry.mjs (artifact target: linux-aarch64-musl)"
        );
    }

    #[test]
    fn artifact_progress_helpers_render_build_and_packaging_steps() {
        assert_eq!(
            format_build_completed_message(Some("linux-aarch64-musl")),
            "Build completed for linux-aarch64-musl"
        );
        assert_eq!(
            format_prepare_artifact_message(Some("linux-aarch64-musl")),
            "Preparing artifact for linux-aarch64-musl"
        );
    }

    #[test]
    fn artifact_progress_helpers_render_shared_messages_without_target_label() {
        assert_eq!(format_build_artifact_message(None), "Building artifact");
        assert_eq!(format_build_completed_message(None), "Build completed");
        assert_eq!(format_prepare_artifact_message(None), "Preparing artifact");
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
        let artifact = Path::new("/repo/examples/js/bun/.tako/artifacts/a.tar.zst");
        assert_eq!(
            format_path_relative_to(project, artifact),
            ".tako/artifacts/a.tar.zst"
        );
    }

    #[test]
    fn format_path_relative_to_falls_back_to_absolute_when_outside_project() {
        let project = Path::new("/repo/examples/js/bun");
        let outside = Path::new("/tmp/a.tar.zst");
        assert_eq!(format_path_relative_to(project, outside), "/tmp/a.tar.zst");
    }

    #[test]
    fn decrypt_deploy_secrets_returns_empty_for_no_secrets() {
        let secrets = SecretsStore::default();
        let result = decrypt_deploy_secrets("my-app", "production", &secrets).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn build_deploy_archive_manifest_includes_tako_build_in_env_vars() {
        let manifest = build_deploy_archive_manifest(
            "my-app",
            "production",
            "v123",
            "bun",
            "server/index.ts",
            300,
            Some("bun install --production".to_string()),
            vec!["bun".to_string(), "run".to_string(), "{main}".to_string()],
            Some("feat: ship it".to_string()),
            Some(false),
            HashMap::new(),
            HashMap::new(),
            None,
        );
        assert_eq!(manifest.idle_timeout, 300);
        assert_eq!(
            manifest.env_vars.get("TAKO_BUILD"),
            Some(&"v123".to_string())
        );
        assert_eq!(
            manifest.env_vars.get("TAKO_ENV"),
            Some(&"production".to_string())
        );
        assert_eq!(
            manifest.install.as_deref(),
            Some("bun install --production")
        );
        assert_eq!(
            manifest.start,
            vec!["bun".to_string(), "run".to_string(), "{main}".to_string()]
        );
        assert_eq!(manifest.commit_message.as_deref(), Some("feat: ship it"));
        assert_eq!(manifest.git_dirty, Some(false));
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
    fn deploy_response_error_detection_only_accepts_structured_status_errors() {
        let json_err = r#"{"status":"error","message":"nope"}"#;
        let json_ok = r#"{"status":"ok","data":{}}"#;
        let old_error_shape = r#"{"error":"old-shape"}"#;
        let plain_text = "all good";

        assert!(deploy_response_has_error(json_err));
        assert!(!deploy_response_has_error(json_ok));
        assert!(!deploy_response_has_error(old_error_shape));
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
    fn cached_artifact_round_trip_verifies_checksum_and_size() {
        let temp = TempDir::new().unwrap();
        let cache_dir = temp.path().join("artifacts");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let paths = artifact_cache_paths(&cache_dir, "abc123", None);

        let artifact_tmp = cache_dir.join("artifact.tmp");
        std::fs::write(&artifact_tmp, b"hello artifact").unwrap();
        let size = std::fs::metadata(&artifact_tmp).unwrap().len();
        persist_cached_artifact(&artifact_tmp, &paths, size).unwrap();

        let verified = load_valid_cached_artifact(&paths).unwrap().unwrap();
        assert_eq!(verified.path, paths.artifact_path);
        assert_eq!(verified.size_bytes, size);
    }

    #[test]
    fn cached_artifact_verification_fails_on_checksum_mismatch() {
        let temp = TempDir::new().unwrap();
        let cache_dir = temp.path().join("artifacts");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let paths = artifact_cache_paths(&cache_dir, "abc123", None);

        std::fs::write(&paths.artifact_path, b"hello artifact").unwrap();
        let bad_metadata = ArtifactCacheMetadata {
            schema_version: ARTIFACT_CACHE_SCHEMA_VERSION,
            artifact_sha256: "deadbeef".to_string(),
            artifact_size: 14,
        };
        std::fs::write(
            &paths.metadata_path,
            serde_json::to_vec_pretty(&bad_metadata).unwrap(),
        )
        .unwrap();

        let err = load_valid_cached_artifact(&paths).unwrap_err();
        assert!(err.contains("checksum mismatch"));
    }

    #[test]
    fn cleanup_local_artifact_cache_prunes_old_artifacts() {
        let temp = TempDir::new().unwrap();
        let cache_dir = temp.path().join("artifacts");
        std::fs::create_dir_all(&cache_dir).unwrap();

        let old_artifact = cache_dir.join("old-version.tar.zst");
        let old_metadata = cache_dir.join("old-version.json");
        let new_artifact = cache_dir.join("new-version.tar.zst");
        let new_metadata = cache_dir.join("new-version.json");
        std::fs::write(&old_artifact, b"old artifact").unwrap();
        std::fs::write(&old_metadata, b"{}").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&new_artifact, b"new artifact").unwrap();
        std::fs::write(&new_metadata, b"{}").unwrap();

        let summary = cleanup_local_artifact_cache(&cache_dir, 1).unwrap();
        assert_eq!(
            summary,
            LocalArtifactCacheCleanupSummary {
                removed_target_artifacts: 1,
                removed_target_metadata: 1,
            }
        );

        assert!(!old_artifact.exists());
        assert!(!old_metadata.exists());
        assert!(new_artifact.exists());
        assert!(new_metadata.exists());
    }

    #[test]
    fn cleanup_local_artifact_cache_prunes_target_subdirectories() {
        let temp = TempDir::new().unwrap();
        let cache_dir = temp.path().join("artifacts");
        let target_dir = cache_dir.join("linux-x86_64-glibc");
        std::fs::create_dir_all(&target_dir).unwrap();

        let old_artifact = target_dir.join("old-version.tar.zst");
        let old_metadata = target_dir.join("old-version.json");
        let new_artifact = target_dir.join("new-version.tar.zst");
        std::fs::write(&old_artifact, b"old").unwrap();
        std::fs::write(&old_metadata, b"{}").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&new_artifact, b"new").unwrap();

        let summary = cleanup_local_artifact_cache(&cache_dir, 1).unwrap();
        assert_eq!(summary.removed_target_artifacts, 1);
        assert!(!old_artifact.exists());
        assert!(new_artifact.exists());
    }

    #[test]
    fn cleanup_local_artifact_cache_removes_orphan_target_metadata() {
        let temp = TempDir::new().unwrap();
        let cache_dir = temp.path().join("artifacts");
        std::fs::create_dir_all(&cache_dir).unwrap();

        let artifact = cache_dir.join("live-version.tar.zst");
        let live_metadata = cache_dir.join("live-version.json");
        let orphan_metadata = cache_dir.join("orphan-version.json");
        std::fs::write(&artifact, b"live artifact").unwrap();
        std::fs::write(&live_metadata, b"{}").unwrap();
        std::fs::write(&orphan_metadata, b"{}").unwrap();

        let summary = cleanup_local_artifact_cache(&cache_dir, 10).unwrap();
        assert_eq!(
            summary,
            LocalArtifactCacheCleanupSummary {
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

        run_local_build(&workspace, "apps/web", "bun", &target_build, &stages).unwrap();
        let order = std::fs::read_to_string(app_dir.join("order.log")).unwrap();
        assert_eq!(
            order,
            "preset-install\npreset-run\nstage-2-run\nstage-3-install\nstage-3-run\n"
        );
    }

    #[test]
    fn build_stage_summary_output_is_hidden_when_empty() {
        let summary: Vec<String> = vec![];
        assert_eq!(format_build_stages_summary_for_output(&summary, None), None);
    }

    #[test]
    fn build_stage_summary_output_is_shown_when_non_empty() {
        let summary = vec!["stage 1 (preset)".to_string(), "stage 2".to_string()];
        assert_eq!(
            format_build_stages_summary_for_output(&summary, Some("linux-x86_64-glibc")),
            Some("Build stages for linux-x86_64-glibc: stage 1 (preset) -> stage 2".to_string())
        );
    }

    #[test]
    fn source_archive_message_is_compact() {
        assert_eq!(
            format_source_archive_created_message(),
            "Source archive created"
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

        let err =
            run_local_build(&workspace, "apps/web", "bun", &target_build, &stages).unwrap_err();
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
    fn save_runtime_version_to_manifest_writes_version_to_app_json() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let app_dir = workspace.join("apps/web");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(
            app_dir.join("app.json"),
            r#"{"runtime":"bun","main":"index.ts","idle_timeout":300}"#,
        )
        .unwrap();

        save_runtime_version_to_manifest(&workspace, "apps/web", "1.3.9").unwrap();

        let manifest_raw = std::fs::read_to_string(app_dir.join("app.json")).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&manifest_raw).unwrap();
        assert_eq!(manifest["runtime_version"], "1.3.9");
        assert_eq!(manifest["runtime"], "bun");
    }

    #[test]
    fn save_runtime_version_cleans_up_old_version_file() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let app_dir = workspace.join("apps/web");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(
            app_dir.join("app.json"),
            r#"{"runtime":"bun","main":"index.ts","idle_timeout":300}"#,
        )
        .unwrap();
        std::fs::write(app_dir.join(RUNTIME_VERSION_OUTPUT_FILE), "1.3.9").unwrap();

        save_runtime_version_to_manifest(&workspace, "apps/web", "1.3.9").unwrap();

        assert!(!app_dir.join(RUNTIME_VERSION_OUTPUT_FILE).exists());
    }

    #[test]
    fn resolve_runtime_version_from_workspace_ignores_old_runtime_version_file() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let app_dir = workspace.join("apps/web");
        std::fs::create_dir_all(&app_dir).unwrap();
        let old_tools_file = format!(".{}{}", "proto", "tools");
        std::fs::write(app_dir.join(old_tools_file), "bun = \"1.3.9\"\n").unwrap();

        let resolved = resolve_runtime_version_from_workspace(&workspace, "apps/web", "bun")
            .expect("resolve runtime version");

        assert_eq!(resolved, "latest");
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
        let cache_paths = artifact_cache_paths(&cache_dir, "v1", Some("linux-aarch64-musl"));
        let archive_size = package_target_artifact(
            &workspace,
            "apps/web",
            &[],
            &["**/*".to_string()],
            &[],
            &cache_paths,
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
    fn package_target_artifact_for_bun_does_not_require_entrypoint_sources() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let app_dir = workspace.join("apps/web");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(app_dir.join("index.ts"), "console.log('ok');").unwrap();
        std::fs::write(app_dir.join("app.json"), r#"{"main":"index.ts"}"#).unwrap();

        let cache_dir = temp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let cache_paths = artifact_cache_paths(&cache_dir, "v1", Some("linux-aarch64-musl"));
        let archive_size = package_target_artifact(
            &workspace,
            "apps/web",
            &[],
            &["**/*".to_string()],
            &[],
            &cache_paths,
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
        let cache_paths = artifact_cache_paths(&cache_dir, "v1", Some("linux-aarch64-musl"));
        let archive_size = package_target_artifact(
            &workspace,
            "apps/web",
            &[],
            &["**/*".to_string()],
            &["**/node_modules/**".to_string()],
            &cache_paths,
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
        let cache_paths = artifact_cache_paths(&cache_dir, "v1", Some("linux-aarch64-musl"));
        let archive_size = package_target_artifact(
            &workspace,
            "apps/web",
            &[],
            &["**/*".to_string()],
            &[],
            &cache_paths,
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
        let cache_paths = artifact_cache_paths(&cache_dir, "v1", Some("linux-aarch64-musl"));
        let err = package_target_artifact(
            &workspace,
            "apps/web",
            &[],
            &["**/*".to_string()],
            &[],
            &cache_paths,
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
    fn remote_release_archive_path_uses_artifacts_tar_zst_name() {
        let path = remote_release_archive_path("/opt/tako/apps/my-app/releases/v1");
        assert_eq!(path, "/opt/tako/apps/my-app/releases/v1/artifacts.tar.zst");
    }

    #[test]
    fn build_remote_extract_archive_command_uses_tako_server_and_cleanup() {
        let cmd = build_remote_extract_archive_command(
            "/opt/tako/apps/a'b/releases/v1",
            "/opt/tako/apps/a'b/releases/v1/artifacts.tar.zst",
        );
        assert!(cmd.contains("tako-server --extract-zstd-archive"));
        assert!(cmd.contains("--extract-dest"));
        assert!(cmd.contains("rm -f"));
        assert!(cmd.contains("'\\''"));
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
            600,
            None,
            vec![],
            None,
            Some(true),
            app_env_vars,
            runtime_env_vars,
            Some(&secrets),
        );

        assert_eq!(manifest.app_name, "my-app");
        assert_eq!(manifest.environment, "staging");
        assert_eq!(manifest.version, "v1");
        assert_eq!(manifest.runtime, "bun");
        assert_eq!(manifest.main, "server/index.mjs");
        assert_eq!(manifest.idle_timeout, 600);
        assert!(manifest.install.is_none());
        assert!(manifest.start.is_empty());
        assert_eq!(manifest.git_dirty, Some(true));
        assert_eq!(
            manifest.env_vars.keys().cloned().collect::<Vec<_>>(),
            vec![
                "A_KEY".to_string(),
                "BUN_ENV".to_string(),
                "NODE_ENV".to_string(),
                "TAKO_BUILD".to_string(),
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
        let temp = TempDir::new().unwrap();
        let config = TakoToml {
            main: Some("server/custom.mjs".to_string()),
            ..Default::default()
        };
        let resolved = resolve_deploy_main(
            temp.path(),
            BuildAdapter::Node,
            &config,
            Some("preset-default.ts"),
        )
        .unwrap();
        assert_eq!(resolved, "server/custom.mjs");
    }

    #[test]
    fn resolve_deploy_main_uses_preset_default_main_when_tako_main_is_missing() {
        let temp = TempDir::new().unwrap();
        let resolved = resolve_deploy_main(
            temp.path(),
            BuildAdapter::Node,
            &TakoToml::default(),
            Some("./dist/server/entry.mjs"),
        )
        .unwrap();
        assert_eq!(resolved, "dist/server/entry.mjs");
    }

    #[test]
    fn resolve_deploy_main_errors_when_tako_and_preset_main_are_missing() {
        let temp = TempDir::new().unwrap();
        let err = resolve_deploy_main(temp.path(), BuildAdapter::Node, &TakoToml::default(), None)
            .unwrap_err();
        assert!(
            err.contains("Set `main` in tako.toml or preset `main`"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn resolve_deploy_main_rejects_parent_directory_segments_from_tako_toml() {
        let temp = TempDir::new().unwrap();
        let config = TakoToml {
            main: Some("../outside.js".to_string()),
            ..Default::default()
        };
        let err = resolve_deploy_main(temp.path(), BuildAdapter::Node, &config, None).unwrap_err();
        assert!(
            err.contains("must not contain '..'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn resolve_deploy_main_rejects_empty_tako_toml_main() {
        let temp = TempDir::new().unwrap();
        let config = TakoToml {
            main: Some("  ".to_string()),
            ..Default::default()
        };
        let err = resolve_deploy_main(temp.path(), BuildAdapter::Node, &config, None).unwrap_err();
        assert!(err.contains("main is empty"), "unexpected error: {err}");
    }

    #[test]
    fn resolve_deploy_main_rejects_invalid_preset_main() {
        let temp = TempDir::new().unwrap();
        let err = resolve_deploy_main(
            temp.path(),
            BuildAdapter::Node,
            &TakoToml::default(),
            Some("../outside.js"),
        )
        .unwrap_err();
        assert!(
            err.contains("must not contain '..'"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn resolve_deploy_main_prefers_root_index_for_js_presets() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("index.tsx"), "export {};\n").unwrap();

        let resolved = resolve_deploy_main(
            temp.path(),
            BuildAdapter::Bun,
            &TakoToml::default(),
            Some("src/index.tsx"),
        )
        .unwrap();

        assert_eq!(resolved, "index.tsx");
    }

    #[test]
    fn resolve_deploy_main_falls_back_to_src_index_when_root_index_is_missing() {
        let temp = TempDir::new().unwrap();
        std::fs::create_dir_all(temp.path().join("src")).unwrap();
        std::fs::write(temp.path().join("src/index.js"), "export {};\n").unwrap();

        let resolved = resolve_deploy_main(
            temp.path(),
            BuildAdapter::Node,
            &TakoToml::default(),
            Some("index.js"),
        )
        .unwrap();

        assert_eq!(resolved, "src/index.js");
    }

    #[test]
    fn resolve_deploy_main_applies_index_fallback_for_deno() {
        let temp = TempDir::new().unwrap();
        std::fs::write(temp.path().join("index.ts"), "export {};\n").unwrap();

        let resolved = resolve_deploy_main(
            temp.path(),
            BuildAdapter::Deno,
            &TakoToml::default(),
            Some("src/index.ts"),
        )
        .unwrap();

        assert_eq!(resolved, "index.ts");
    }
}
