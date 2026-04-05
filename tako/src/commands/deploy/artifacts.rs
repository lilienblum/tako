use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use crate::build::{BuildAdapter, BuildCache, BuildExecutor, BuildPreset, PresetGroup};
use crate::config::{BuildStage, TakoToml};
use crate::output;

use super::BuildPhaseResult;
use super::cache::{
    ArtifactCachePaths, artifact_cache_paths, artifact_cache_temp_path,
    cleanup_local_artifact_cache, cleanup_local_build_workspaces, load_valid_cached_artifact,
    persist_cached_artifact, remove_cached_artifact_files, sanitize_cache_label,
};
use super::format::{
    format_artifact_cache_hit_message_for_output, format_artifact_cache_invalid_message,
    format_artifact_ready_message, format_artifact_ready_message_for_output,
    format_build_artifact_message, format_build_artifact_success, format_build_completed_message,
    format_build_stages_summary_for_output, format_path_relative_to,
    format_prepare_artifact_message, format_prepare_artifact_success, format_runtime_probe_message,
    format_runtime_probe_success, format_size, format_stage_label, should_use_local_build_spinners,
};
use super::manifest::{
    DEPLOY_ARCHIVE_MANIFEST_FILE, build_deploy_archive_manifest, decrypt_deploy_secrets,
    resolve_deploy_main, resolve_deploy_version_and_source_hash, resolve_git_commit_message,
};
use super::task_tree::{ArtifactBuildGroup, DeployTaskTreeController};

pub(super) const LOCAL_BUILD_WORKSPACE_RELATIVE_DIR: &str = ".tako/tmp/workspaces";
pub(super) const LOCAL_BUILD_CACHE_RELATIVE_DIR: &str = ".tako/tmp/build-caches";
pub(super) const RUNTIME_VERSION_OUTPUT_FILE: &str = ".tako-runtime-version";
pub(super) const LOCAL_ARTIFACT_CACHE_KEEP_TARGET_ARTIFACTS: usize = 90;

#[derive(Clone, Copy)]
enum LocalBuildCacheScope {
    Workspace,
    App,
}

#[derive(Clone, Copy)]
struct LocalBuildCacheSpec {
    relative_path: &'static str,
    scope: LocalBuildCacheScope,
}

const JS_LOCAL_BUILD_CACHE_SPECS: &[LocalBuildCacheSpec] = &[
    LocalBuildCacheSpec {
        relative_path: ".turbo",
        scope: LocalBuildCacheScope::Workspace,
    },
    LocalBuildCacheSpec {
        relative_path: ".next/cache",
        scope: LocalBuildCacheScope::App,
    },
];

#[allow(clippy::too_many_arguments)]
pub(super) async fn prepare_build_phase(
    project_dir: PathBuf,
    source_root: PathBuf,
    eff_app_dir: PathBuf,
    app_name: String,
    env: String,
    tako_config: TakoToml,
    secrets: crate::config::SecretsStore,
    preset_ref: String,
    runtime_adapter: BuildAdapter,
    server_targets: Vec<(String, crate::config::ServerTarget)>,
    build_groups: Vec<ArtifactBuildGroup>,
    task_tree: Option<DeployTaskTreeController>,
) -> Result<BuildPhaseResult, String> {
    let phase = if task_tree.is_none() {
        Some(output::PhaseSpinner::start("Building…"))
    } else {
        None
    };
    let build_phase_timer = output::timed("Build phase");

    let executor = BuildExecutor::new(&project_dir);
    let cache = BuildCache::new(project_dir.join(".tako/artifacts"));
    cache.init().map_err(|e| e.to_string())?;
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
        Err(error) => {
            if task_tree.is_none() {
                output::warning(&format!("Local artifact cache cleanup skipped: {}", error));
            } else {
                tracing::warn!("Local artifact cache cleanup skipped: {}", error);
            }
        }
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
        Err(error) => {
            if task_tree.is_none() {
                output::warning(&format!("Local build workspace cleanup skipped: {}", error));
            } else {
                tracing::warn!("Local build workspace cleanup skipped: {}", error);
            }
        }
    }

    let (version, _source_hash) = resolve_deploy_version_and_source_hash(&executor, &source_root)
        .map_err(|e| e.to_string())?;
    let git_commit_message = resolve_git_commit_message(&source_root);
    let git_dirty = executor.is_git_dirty().ok();
    tracing::debug!("Version: {}", version);
    tracing::debug!("Resolving preset ref: {}…", preset_ref);

    let (mut build_preset, resolved_preset) = {
        let _t = output::timed("Preset resolution");
        if task_tree.is_none() {
            output::with_spinner_async(
                "Resolving build preset",
                "Build preset resolved",
                crate::build::load_build_preset(&eff_app_dir, &preset_ref),
            )
            .await
            .map_err(|e| e.to_string())?
        } else {
            crate::build::load_build_preset(&eff_app_dir, &preset_ref)
                .await
                .map_err(|e| e.to_string())?
        }
    };
    tracing::debug!(
        "Resolved preset: {} (commit {})",
        resolved_preset.preset_ref,
        super::format::shorten_commit(&resolved_preset.commit)
    );

    let plugin_ctx = tako_runtime::PluginContext {
        project_dir: &eff_app_dir,
        package_manager: tako_config.package_manager.as_deref(),
    };
    crate::build::apply_adapter_base_runtime_defaults(
        &mut build_preset,
        runtime_adapter,
        Some(&plugin_ctx),
    )
    .map_err(|e| e.to_string())?;
    tracing::debug!(
        "Build preset: {} @ {}",
        resolved_preset.preset_ref,
        super::format::shorten_commit(&resolved_preset.commit)
    );
    tracing::debug!(
        "{}",
        super::format::format_runtime_summary(&build_preset.name, None)
    );
    let runtime_tool = runtime_adapter.id().to_string();

    let manifest_main = resolve_deploy_main(
        &eff_app_dir,
        runtime_adapter,
        &tako_config,
        build_preset.main.as_deref(),
    )?;
    tracing::debug!(
        "{}",
        super::format::format_entry_point_summary(&eff_app_dir.join(&manifest_main),)
    );

    let env_idle_timeout = tako_config.get_idle_timeout(&env);
    let app_dir = project_dir
        .strip_prefix(&source_root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let runtime_proj_root =
        tako_runtime::find_runtime_project_root(runtime_adapter.id(), &project_dir);
    let install_dir = runtime_proj_root
        .strip_prefix(&source_root)
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();

    let deploy_pm = tako_config
        .package_manager
        .clone()
        .or_else(|| tako_runtime::read_package_manager_spec(&eff_app_dir))
        .or_else(|| {
            tako_runtime::detect_package_manager(&eff_app_dir).map(|pm| pm.id().to_string())
        })
        .or_else(|| {
            tako_runtime::detect_package_manager(&runtime_proj_root).map(|pm| pm.id().to_string())
        });

    let manifest = build_deploy_archive_manifest(
        &app_name,
        &env,
        &version,
        runtime_adapter.id(),
        &manifest_main,
        env_idle_timeout,
        deploy_pm,
        git_commit_message.clone(),
        git_dirty,
        tako_config.get_merged_vars(&env),
        HashMap::new(),
        secrets.get_env(&env),
        app_dir,
        install_dir,
    );
    let deploy_secrets =
        decrypt_deploy_secrets(&app_name, &env, &secrets).map_err(|e| e.to_string())?;

    let source_archive_dir = project_dir.join(".tako/tmp");
    std::fs::create_dir_all(&source_archive_dir).map_err(|e| e.to_string())?;
    let source_archive_path = source_archive_dir.join("source.tar.zst");
    let app_json_bytes = serde_json::to_vec_pretty(&manifest).map_err(|e| e.to_string())?;
    let app_manifest_archive_path = DEPLOY_ARCHIVE_MANIFEST_FILE.to_string();
    let source_archive_size = if task_tree.is_none() {
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
        .map_err(|e| e.to_string())?
    } else {
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
        size.map_err(|e| e.to_string())?
    };

    tracing::debug!(
        "Source archive created: {} ({})",
        format_path_relative_to(&project_dir, &source_archive_path),
        format_size(source_archive_size),
    );

    let include_patterns = build_artifact_include_patterns(&tako_config);
    let exclude_patterns = build_artifact_exclude_patterns(&build_preset, &tako_config);
    let asset_roots = build_asset_roots(&build_preset, &tako_config)?;

    if let Some(server_targets_summary) = super::format::format_server_targets_summary(
        &server_targets,
        super::format::should_use_unified_js_target_process(&runtime_tool),
    ) {
        tracing::debug!("{}", server_targets_summary);
    }

    let artifacts_by_target = build_target_artifacts(
        &project_dir,
        &source_root,
        &tako_config,
        cache.cache_dir(),
        &build_workspace_root,
        &source_archive_path,
        &app_json_bytes,
        &version,
        &runtime_tool,
        &manifest_main,
        &build_groups,
        &tako_config.build_stages,
        &include_patterns,
        &exclude_patterns,
        &asset_roots,
        tako_config.runtime_version.as_deref(),
        task_tree.clone(),
    )
    .await?;

    drop(build_phase_timer);
    if let Some(phase) = phase {
        phase.finish("Built");
    }

    Ok(BuildPhaseResult {
        version,
        manifest_main,
        deploy_secrets,
        use_unified_target_process: super::format::should_use_unified_js_target_process(
            &runtime_tool,
        ),
        artifacts_by_target,
    })
}

pub(super) fn build_artifact_include_patterns(config: &TakoToml) -> Vec<String> {
    // Stage `include` patterns are additive — they specify build outputs to
    // keep alongside the app's own files, not an exclusive filter.  The workdir
    // is already gitignore-filtered, so we include everything.
    if !config.build_stages.is_empty() {
        return vec!["**/*".to_string()];
    }
    if !config.build.include.is_empty() {
        return config.build.include.clone();
    }
    vec!["**/*".to_string()]
}

#[cfg(test)]
pub(super) fn should_report_artifact_include_patterns(include_patterns: &[String]) -> bool {
    if include_patterns.is_empty() {
        return false;
    }
    !(include_patterns.len() == 1 && include_patterns[0] == "**/*")
}

pub(super) fn build_artifact_exclude_patterns(
    _preset: &BuildPreset,
    config: &TakoToml,
) -> Vec<String> {
    if !config.build_stages.is_empty() {
        let mut patterns = Vec::new();
        for stage in &config.build_stages {
            for exclude in &stage.exclude {
                match &stage.cwd {
                    Some(cwd) if !cwd.is_empty() && cwd != "." => {
                        patterns.push(format!("{}/{}", cwd.trim_end_matches('/'), exclude));
                    }
                    _ => {
                        patterns.push(exclude.clone());
                    }
                }
            }
        }
        return patterns;
    }
    config.build.exclude.clone()
}

pub(super) fn build_asset_roots(
    preset: &BuildPreset,
    config: &TakoToml,
) -> Result<Vec<String>, String> {
    let mut merged = Vec::new();
    for root in preset.assets.iter().chain(config.assets.iter()) {
        let normalized = normalize_asset_root(root)?;
        if !merged.contains(&normalized) {
            merged.push(normalized);
        }
    }
    Ok(merged)
}

pub(super) fn normalize_asset_root(asset_root: &str) -> Result<String, String> {
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

fn local_build_cache_specs(runtime_adapter: BuildAdapter) -> &'static [LocalBuildCacheSpec] {
    if runtime_adapter.preset_group() == PresetGroup::Js {
        JS_LOCAL_BUILD_CACHE_SPECS
    } else {
        &[]
    }
}

fn local_build_cache_root(project_dir: &Path, cache_target_label: &str) -> PathBuf {
    project_dir
        .join(LOCAL_BUILD_CACHE_RELATIVE_DIR)
        .join(sanitize_cache_label(cache_target_label))
}

fn restore_local_build_caches(
    cache_root: &Path,
    workspace_root: &Path,
    app_dir: &Path,
    runtime_adapter: BuildAdapter,
) -> Result<usize, String> {
    let mut restored = 0usize;
    for spec in local_build_cache_specs(runtime_adapter) {
        let source = cache_root.join(spec.relative_path);
        if !source.is_dir() {
            continue;
        }
        let destination = local_build_cache_destination(spec, workspace_root, app_dir);
        replace_directory_from_cache(&source, &destination)?;
        restored += 1;
    }
    Ok(restored)
}

fn persist_local_build_caches(
    cache_root: &Path,
    workspace_root: &Path,
    app_dir: &Path,
    runtime_adapter: BuildAdapter,
) -> Result<usize, String> {
    let mut persisted = 0usize;
    for spec in local_build_cache_specs(runtime_adapter) {
        let source = local_build_cache_destination(spec, workspace_root, app_dir);
        if !source.is_dir() {
            continue;
        }
        let destination = cache_root.join(spec.relative_path);
        replace_directory_from_cache(&source, &destination)?;
        persisted += 1;
    }
    Ok(persisted)
}

fn local_build_cache_destination(
    spec: &LocalBuildCacheSpec,
    workspace_root: &Path,
    app_dir: &Path,
) -> PathBuf {
    match spec.scope {
        LocalBuildCacheScope::Workspace => workspace_root.join(spec.relative_path),
        LocalBuildCacheScope::App => app_dir.join(spec.relative_path),
    }
}

fn replace_directory_from_cache(source: &Path, destination: &Path) -> Result<(), String> {
    remove_path_if_exists(destination)?;
    std::fs::create_dir_all(destination)
        .map_err(|e| format!("Failed to create {}: {e}", destination.display()))?;
    copy_dir_contents(source, destination)
}

fn remove_path_if_exists(path: &Path) -> Result<(), String> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("Failed to stat {}: {error}", path.display())),
    };

    if metadata.file_type().is_symlink() || metadata.is_file() {
        std::fs::remove_file(path)
            .map_err(|e| format!("Failed to remove {}: {e}", path.display()))?;
        return Ok(());
    }

    std::fs::remove_dir_all(path).map_err(|e| format!("Failed to remove {}: {e}", path.display()))
}

#[allow(clippy::too_many_arguments)]
async fn build_target_artifacts(
    project_dir: &Path,
    source_root: &Path,
    tako_config: &TakoToml,
    cache_dir: &Path,
    _build_workspace_root: &Path,
    _source_archive_path: &Path,
    app_manifest_bytes: &[u8],
    version: &str,
    runtime_tool: &str,
    _main: &str,
    target_groups: &[ArtifactBuildGroup],
    custom_stages: &[BuildStage],
    include_patterns: &[String],
    exclude_patterns: &[String],
    asset_roots: &[String],
    pinned_runtime_version: Option<&str>,
    task_tree: Option<DeployTaskTreeController>,
) -> Result<HashMap<String, PathBuf>, String> {
    let has_multiple_targets = target_groups.len() > 1;
    let mut artifacts = HashMap::new();
    let runtime_adapter = BuildAdapter::from_id(runtime_tool).unwrap_or(BuildAdapter::Unknown);

    for target_group in target_groups.iter().cloned() {
        let build_target_label = target_group.build_target_label;
        let cache_target_label = target_group.cache_target_label;
        let build_cache_root = local_build_cache_root(project_dir, &cache_target_label);
        let display_target_label = target_group.display_target_label.as_deref();
        let tree_target_label = display_target_label.unwrap_or("shared target").to_string();
        let use_local_build_spinners =
            task_tree.is_none() && should_use_local_build_spinners(output::is_interactive());
        let stage_summary = summarize_build_stages(custom_stages);
        if let Some(stage_summary_message) =
            format_build_stages_summary_for_output(&stage_summary, display_target_label)
        {
            tracing::debug!("{}", stage_summary_message);
        }
        // Clean workdir from any previous build, then create a fresh copy of the source tree
        // respecting .gitignore. Symlink node_modules so workspace references resolve.
        let workdir = project_dir.join(".tako/workdir");
        crate::build::cleanup_workdir(&workdir);
        {
            let _t = output::timed("Workdir setup");
            crate::build::create_workdir(source_root, &workdir)
                .map_err(|e| format!("Failed to create workdir: {e}"))?;
            if runtime_adapter.preset_group() == PresetGroup::Js {
                crate::build::symlink_node_modules(source_root, &workdir)
                    .map_err(|e| format!("Failed to symlink node_modules: {e}"))?;
            }
        }
        let workspace = workdir.clone();
        let app_dir_in_workspace = match project_dir.strip_prefix(source_root) {
            Ok(rel) if !rel.as_os_str().is_empty() => workspace.join(rel),
            _ => workspace.clone(),
        };

        match restore_local_build_caches(
            &build_cache_root,
            &workspace,
            &app_dir_in_workspace,
            runtime_adapter,
        ) {
            Ok(restored) if restored > 0 => {
                tracing::debug!(
                    "Restored {} local build cache director{} for {}",
                    restored,
                    if restored == 1 { "y" } else { "ies" },
                    cache_target_label
                );
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(
                    "Failed to restore local build caches for {}: {}",
                    cache_target_label,
                    error
                );
            }
        }

        // Write app.json at the workdir root (project dir = archive root).
        {
            std::fs::write(workspace.join("app.json"), app_manifest_bytes)
                .map_err(|e| format!("Failed to write app.json: {e}"))?;
        }

        let runtime_probe_label = format_runtime_probe_message(display_target_label);
        let runtime_probe_success = format_runtime_probe_success(display_target_label);
        let runtime_version = if let Some(pinned) = pinned_runtime_version {
            tracing::debug!("Using pinned runtime version {} from tako.toml", pinned);
            if let Some(task_tree) = &task_tree {
                task_tree.warn_build_step(
                    &tree_target_label,
                    "probe-runtime",
                    format!("Pinned: {pinned}"),
                );
            }
            pinned.to_string()
        } else if task_tree.is_some() {
            if let Some(task_tree) = &task_tree {
                task_tree.mark_build_step_running(&tree_target_label, "probe-runtime");
            }
            let version_result =
                resolve_runtime_version_from_workspace_quiet(&workspace, runtime_tool);
            match version_result {
                Ok(version) => {
                    if let Some(task_tree) = &task_tree {
                        task_tree.succeed_build_step(
                            &tree_target_label,
                            "probe-runtime",
                            Some(version.clone()),
                        );
                    }
                    version
                }
                Err(error) => {
                    if let Some(task_tree) = &task_tree {
                        task_tree.fail_build_step(
                            &tree_target_label,
                            "probe-runtime",
                            error.clone(),
                        );
                        task_tree.fail_build_target(&tree_target_label, error.clone());
                        task_tree.warn_pending_build_children(&tree_target_label, "skipped");
                    }
                    return Err(error);
                }
            }
        } else if use_local_build_spinners {
            output::with_spinner(&runtime_probe_label, &runtime_probe_success, || {
                tracing::debug!(
                    "Probing {} version in {}…",
                    runtime_tool,
                    workspace.display()
                );
                let _t = output::timed("Runtime probe");
                let version = resolve_runtime_version_from_workspace(&workspace, runtime_tool);
                if let Ok(v) = &version {
                    tracing::debug!("Detected {} {}", runtime_tool, v);
                }
                version
            })?
        } else {
            tracing::debug!("{}", runtime_probe_label);
            let _t = output::timed("Runtime probe");
            let version = resolve_runtime_version_from_workspace(&workspace, runtime_tool)?;
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
                if let Some(task_tree) = &task_tree {
                    task_tree.warn_build_step(&tree_target_label, "build-artifact", "skipped");
                    task_tree.warn_build_step(&tree_target_label, "package-artifact", "skipped");
                    task_tree.append_cached_artifact_step(
                        &tree_target_label,
                        Some(format_size(cached.size_bytes)),
                    );
                    task_tree.succeed_build_target(
                        &tree_target_label,
                        Some(format!("{} (cached)", format_size(cached.size_bytes))),
                    );
                } else if has_multiple_targets {
                    output::bullet(&format_build_completed_message(display_target_label));
                } else {
                    output::bullet(&format_artifact_cache_hit_message_for_output(
                        display_target_label,
                    ));
                }
                for target_label in &target_group.target_labels {
                    artifacts.insert(target_label.clone(), cached.path.clone());
                }
                continue;
            }
            Ok(None) => {
                tracing::debug!("Artifact cache miss, building from source");
            }
            Err(error) => {
                if task_tree.is_none() {
                    output::warning(&format_artifact_cache_invalid_message(
                        display_target_label,
                        &error,
                    ));
                } else {
                    tracing::warn!(
                        "{}",
                        format_artifact_cache_invalid_message(display_target_label, &error)
                    );
                }
                remove_cached_artifact_files(&cache_paths);
            }
        }

        // For Go builds, inject GOOS/GOARCH for cross-compilation to the
        // target server. Deploy targets are always Linux; the arch is derived
        // from the build_target_label (e.g. "linux-aarch64-glibc").
        let go_cross_envs: Vec<(&str, String)> = if runtime_adapter.preset_group()
            == PresetGroup::Go
        {
            let goarch =
                if build_target_label.contains("aarch64") || build_target_label.contains("arm64") {
                    "arm64"
                } else {
                    "amd64"
                };
            vec![
                ("GOOS", "linux".to_string()),
                ("GOARCH", goarch.to_string()),
            ]
        } else {
            Vec::new()
        };
        let extra_envs: Vec<(&str, &str)> = go_cross_envs
            .iter()
            .map(|(k, v)| (*k, v.as_str()))
            .collect();

        let build_result = (|| -> Result<u64, String> {
            let build_label = format_build_artifact_message(display_target_label);
            let build_success = format_build_artifact_success(display_target_label);
            if let Some(task_tree) = &task_tree {
                task_tree.mark_build_step_running(&tree_target_label, "build-artifact");
                if let Err(error) = run_local_build(
                    &workspace,
                    &app_dir_in_workspace,
                    source_root,
                    project_dir,
                    &tako_config.build,
                    custom_stages,
                    &extra_envs,
                ) {
                    task_tree.fail_build_step(&tree_target_label, "build-artifact", error.clone());
                    task_tree.fail_build_target(&tree_target_label, error.clone());
                    task_tree.warn_pending_build_children(&tree_target_label, "skipped");
                    return Err(error);
                }
                task_tree.succeed_build_step(&tree_target_label, "build-artifact", None);
            } else if use_local_build_spinners {
                output::with_spinner(&build_label, &build_success, || {
                    tracing::debug!("Building target {}…", build_target_label);
                    let _t = output::timed("Target build");
                    run_local_build(
                        &workspace,
                        &app_dir_in_workspace,
                        source_root,
                        project_dir,
                        &tako_config.build,
                        custom_stages,
                        &extra_envs,
                    )
                })?;
            } else {
                output::bullet(&build_label);
                let _t = output::timed("Target build");
                run_local_build(
                    &workspace,
                    &app_dir_in_workspace,
                    source_root,
                    project_dir,
                    &tako_config.build,
                    custom_stages,
                    &extra_envs,
                )?;
            }
            save_runtime_version_to_manifest(&workspace, &runtime_version)?;
            match persist_local_build_caches(
                &build_cache_root,
                &workspace,
                &app_dir_in_workspace,
                runtime_adapter,
            ) {
                Ok(persisted) if persisted > 0 => {
                    tracing::debug!(
                        "Saved {} local build cache director{} for {}",
                        persisted,
                        if persisted == 1 { "y" } else { "ies" },
                        cache_target_label
                    );
                }
                Ok(_) => {}
                Err(error) => {
                    tracing::warn!(
                        "Failed to persist local build caches for {}: {}",
                        cache_target_label,
                        error
                    );
                }
            }
            tracing::debug!("{}", format_build_completed_message(display_target_label));

            let prepare_label = format_prepare_artifact_message(display_target_label);
            let prepare_success = format_prepare_artifact_success(display_target_label);
            if let Some(task_tree) = &task_tree {
                task_tree.mark_build_step_running(&tree_target_label, "package-artifact");
                if let Err(error) = package_target_artifact(
                    &workspace,
                    &app_dir_in_workspace,
                    asset_roots,
                    include_patterns,
                    exclude_patterns,
                    &cache_paths,
                    &build_target_label,
                ) {
                    task_tree.fail_build_step(
                        &tree_target_label,
                        "package-artifact",
                        error.clone(),
                    );
                    task_tree.fail_build_target(&tree_target_label, error.clone());
                    return Err(error);
                }
                Ok(cache_paths
                    .artifact_path
                    .metadata()
                    .map_err(|e| format!("Failed to read artifact metadata: {e}"))?
                    .len())
            } else if use_local_build_spinners {
                output::with_spinner(&prepare_label, &prepare_success, || {
                    tracing::debug!("Packaging artifact for {}…", build_target_label);
                    let _t = output::timed("Artifact packaging");
                    package_target_artifact(
                        &workspace,
                        &app_dir_in_workspace,
                        asset_roots,
                        include_patterns,
                        exclude_patterns,
                        &cache_paths,
                        &build_target_label,
                    )
                })
            } else {
                output::bullet(&prepare_label);
                tracing::debug!("Packaging artifact for {}…", build_target_label);
                let _t = output::timed("Artifact packaging");
                package_target_artifact(
                    &workspace,
                    &app_dir_in_workspace,
                    asset_roots,
                    include_patterns,
                    exclude_patterns,
                    &cache_paths,
                    &build_target_label,
                )
            }
        })();
        let artifact_size = build_result?;

        if let Some(task_tree) = &task_tree {
            task_tree.succeed_build_step(
                &tree_target_label,
                "package-artifact",
                Some(format_size(artifact_size)),
            );
            task_tree.succeed_build_target(&tree_target_label, Some(format_size(artifact_size)));
        }

        tracing::debug!(
            "{}",
            format_artifact_ready_message_for_output(display_target_label,)
        );
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
        if task_tree.is_none() && has_multiple_targets {
            output::bullet(&format_build_completed_message(display_target_label));
        }

        // Clean up workdir after packaging.
        crate::build::cleanup_workdir(&workdir);
    }

    Ok(artifacts)
}

pub(super) fn run_local_build(
    workspace: &Path,
    app_dir: &Path,
    original_source_root: &Path,
    original_app_dir: &Path,
    build_config: &crate::config::BuildConfig,
    custom_stages: &[BuildStage],
    extra_envs: &[(&str, &str)],
) -> Result<(), String> {
    if !workspace.is_dir() {
        return Err(format!(
            "App directory '{}' does not exist inside build workspace",
            workspace.display()
        ));
    }
    // Resolve the working directory for build commands.
    // `build.cwd` is relative to the workspace root (source root).
    // Default: the app directory within the workspace.
    let build_run_dir = match build_config.cwd.as_deref() {
        Some(cwd) if !cwd.is_empty() && cwd != "." => {
            let dir = workspace.join(cwd);
            if !dir.is_dir() {
                return Err(format!(
                    "build.cwd directory '{}' does not exist inside build workspace",
                    cwd
                ));
            }
            dir
        }
        Some(_) => workspace.to_path_buf(), // "." means workspace root
        None => app_dir.to_path_buf(),
    };

    let has_build_run = build_config
        .run
        .as_deref()
        .map(str::trim)
        .is_some_and(|s| !s.is_empty());

    if !has_build_run && custom_stages.is_empty() {
        return Ok(());
    }

    let app_dir_value = workspace.to_string_lossy().to_string();

    let run_shell =
        |cwd: &Path, command: &str, phase: &str, stage_label: &str| -> Result<(), String> {
            let mut cmd = std::process::Command::new("sh");
            cmd.args(["-lc", command])
                .current_dir(cwd)
                .env("TAKO_APP_DIR", &app_dir_value);
            for (key, value) in extra_envs {
                cmd.env(key, value);
            }
            let output = cmd
                .stdin(std::process::Stdio::null())
                .output()
                .map_err(|e| format!("Failed to run local {stage_label} {phase} command: {e}"))?;
            if output.status.success() {
                return Ok(());
            }
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let detail = if stderr.is_empty() { stdout } else { stderr };
            Err(format!("{stage_label} {phase} command failed: {detail}"))
        };

    // Run [build].install then [build].run if configured (simple build mode).
    if has_build_run {
        if let Some(install) = build_config
            .install
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            tracing::debug!("Running build install: {install}…");
            let _t = output::timed("build install");
            run_shell(&build_run_dir, install, "install", "build")?;
        }
        let run_command = build_config.run.as_deref().unwrap().trim();
        tracing::debug!("Running build: {run_command}…");
        let _t = output::timed("build");
        run_shell(&build_run_dir, run_command, "run", "build")?;
    }

    // Run [[build_stages]] in order (multi-stage mode).
    let mut stage_number = 1usize;
    for stage in custom_stages {
        let stage_label = format_stage_label(stage_number, stage.name.as_deref());
        let stage_cwd = resolve_stage_working_dir_for_local_build(
            original_source_root,
            original_app_dir,
            workspace,
            stage.cwd.as_deref(),
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
            return Err(format!("{stage_label} run command is empty"));
        }
        tracing::debug!("Running {stage_label}: {run_command}…");
        let _t = output::timed(&stage_label);
        run_shell(&stage_cwd, run_command, "run", &stage_label)?;
        drop(_t);
        stage_number += 1;
    }

    Ok(())
}

pub(super) fn summarize_build_stages(custom_stages: &[BuildStage]) -> Vec<String> {
    let mut labels = Vec::new();
    let mut stage_number = 1;
    for stage in custom_stages {
        labels.push(format_stage_label(stage_number, stage.name.as_deref()));
        stage_number += 1;
    }
    labels
}

fn resolve_stage_working_dir_for_local_build(
    original_source_root: &Path,
    original_app_dir: &Path,
    workspace: &Path,
    working_dir: Option<&str>,
    stage_label: &str,
) -> Result<PathBuf, String> {
    let Some(working_dir) = working_dir.map(str::trim).filter(|value| !value.is_empty()) else {
        let relative_app = original_app_dir
            .strip_prefix(original_source_root)
            .unwrap_or(Path::new(""));
        return Ok(workspace.join(relative_app));
    };
    // Check escape: normalize the path from source root through app dir to stage cwd.
    let relative_app = original_app_dir
        .strip_prefix(original_source_root)
        .unwrap_or(Path::new(""));
    let full_relative = relative_app.join(working_dir);
    let mut depth: i32 = 0;
    for component in full_relative.components() {
        match component {
            std::path::Component::ParentDir => depth -= 1,
            std::path::Component::Normal(_) => depth += 1,
            _ => {}
        }
        if depth < 0 {
            return Err(format!(
                "{stage_label} working directory '{working_dir}' must not escape the project root",
            ));
        }
    }
    // Resolve against original app dir to validate existence.
    let resolved = original_app_dir.join(working_dir);
    if !resolved.is_dir() {
        return Err(format!(
            "{stage_label} working directory '{working_dir}' not found",
        ));
    }
    // Map into workdir via canonicalized relative path.
    let canonical = resolved
        .canonicalize()
        .map_err(|_| format!("{stage_label} working directory '{working_dir}' not found"))?;
    let canonical_root = original_source_root.canonicalize().map_err(|e| {
        format!(
            "Failed to resolve source root '{}': {e}",
            original_source_root.display()
        )
    })?;
    let relative = canonical.strip_prefix(&canonical_root).unwrap();
    Ok(workspace.join(relative))
}

/// Save the resolved runtime version into the deploy manifest (`app.json`).
pub(super) fn save_runtime_version_to_manifest(
    workspace: &Path,
    runtime_version: &str,
) -> Result<(), String> {
    let manifest_path = workspace.join("app.json");
    let content = std::fs::read_to_string(&manifest_path)
        .map_err(|e| format!("Failed to read {}: {e}", manifest_path.display()))?;
    let mut value: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("Failed to parse {}: {e}", manifest_path.display()))?;
    value["runtime_version"] = serde_json::Value::String(runtime_version.to_string());
    let updated = serde_json::to_string_pretty(&value)
        .map_err(|e| format!("Failed to serialize {}: {e}", manifest_path.display()))?;
    std::fs::write(&manifest_path, updated)
        .map_err(|e| format!("Failed to write {}: {e}", manifest_path.display()))?;
    let _ = std::fs::remove_file(workspace.join(RUNTIME_VERSION_OUTPUT_FILE));
    Ok(())
}

/// Extract a semver version from `--version` output.
/// Handles formats like "bun 1.3.11", "deno 2.7.6 (stable, ...)", "v22.12.0",
/// and pre-release versions like "1.0.0-rc1".
pub(super) fn extract_semver_from_version_output(output: &str) -> Option<String> {
    let line = output.lines().map(str::trim).find(|l| !l.is_empty())?;
    for word in line.split_whitespace() {
        // Strip non-digit prefixes: 'v' for node/bun/deno, 'go' for Go
        let word = word.trim_start_matches(|c: char| !c.is_ascii_digit());
        if word.chars().next().is_some_and(|c| c.is_ascii_digit())
            && word.contains('.')
            && word
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '+')
        {
            return Some(word.to_string());
        }
    }
    None
}

fn resolve_runtime_version_from_workspace(
    workspace: &Path,
    runtime_tool: &str,
) -> Result<String, String> {
    resolve_runtime_version_from_workspace_impl(workspace, runtime_tool, true)
}

fn resolve_runtime_version_from_workspace_quiet(
    workspace: &Path,
    runtime_tool: &str,
) -> Result<String, String> {
    resolve_runtime_version_from_workspace_impl(workspace, runtime_tool, false)
}

fn resolve_runtime_version_from_workspace_impl(
    workspace: &Path,
    runtime_tool: &str,
    emit_warning: bool,
) -> Result<String, String> {
    if !workspace.is_dir() {
        return Err(format!(
            "App directory '{}' does not exist inside build workspace",
            workspace.display()
        ));
    }

    #[cfg(test)]
    {
        let _ = (workspace, runtime_tool, emit_warning);
        Ok("latest".to_string())
    }

    #[cfg(not(test))]
    {
        let command = format!(
            "{} --version",
            crate::shell::shell_single_quote(runtime_tool)
        );
        let output = std::process::Command::new("sh")
            .args(["-lc", &command])
            .current_dir(workspace)
            .stdin(std::process::Stdio::null())
            .output();
        match output {
            Ok(out) if out.status.success() => {
                let stdout = String::from_utf8_lossy(&out.stdout);
                if let Some(version) = extract_semver_from_version_output(&stdout) {
                    return Ok(version);
                }
                if emit_warning {
                    output::warning(&format!(
                        "Could not detect {runtime_tool} version. To pin a version, set runtime_version in tako.toml"
                    ));
                }
                Ok("latest".to_string())
            }
            _ => {
                if emit_warning {
                    output::warning(&format!(
                        "Could not detect {runtime_tool} version. To pin a version, set runtime_version in tako.toml"
                    ));
                }
                Ok("latest".to_string())
            }
        }
    }
}

pub(super) fn merge_assets_locally(
    workspace_root: &Path,
    asset_roots: &[String],
) -> Result<(), String> {
    if asset_roots.is_empty() {
        return Ok(());
    }

    if !workspace_root.is_dir() {
        return Err(format!(
            "App directory '{}' does not exist inside build workspace",
            workspace_root.display()
        ));
    }

    let public_dir = workspace_root.join("public");
    std::fs::create_dir_all(&public_dir)
        .map_err(|e| format!("Failed to create {}: {e}", public_dir.display()))?;

    for asset_root in asset_roots {
        if asset_root == "public" {
            continue;
        }
        let src = workspace_root.join(asset_root);
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

pub(super) fn package_target_artifact(
    workspace: &Path,
    app_dir: &Path,
    asset_roots: &[String],
    include_patterns: &[String],
    exclude_patterns: &[String],
    cache_paths: &ArtifactCachePaths,
    target_label: &str,
) -> Result<u64, String> {
    merge_assets_locally(app_dir, asset_roots)?;

    let artifact_temp_path = artifact_cache_temp_path(&cache_paths.artifact_path)?;
    let artifact_size = crate::build::create_workdir_archive(
        workspace,
        &artifact_temp_path,
        include_patterns,
        exclude_patterns,
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

pub(super) fn copy_dir_contents(src: &Path, dst: &Path) -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    use super::super::cache::artifact_cache_paths;
    use super::*;
    use crate::build::{BuildExecutor, BuildPreset};
    use tempfile::TempDir;

    #[test]
    fn normalize_asset_root_rejects_invalid_paths() {
        assert!(normalize_asset_root(" ").is_err());
        assert!(normalize_asset_root("/tmp/assets").is_err());
        assert!(normalize_asset_root("../assets").is_err());
    }

    #[test]
    fn build_asset_roots_combines_and_deduplicates_preset_and_project_values() {
        let preset = BuildPreset {
            name: "bun".to_string(),
            main: None,
            assets: vec!["public".to_string(), "dist/client".to_string()],
            dev: vec![],
        };
        let config = TakoToml {
            assets: vec!["dist/client".to_string(), "assets/shared".to_string()],
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
                ..Default::default()
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
    fn build_artifact_include_patterns_stages_include_everything() {
        let mut config = TakoToml::default();
        config.build_stages = vec![
            crate::config::BuildStage {
                name: Some("rust".to_string()),
                cwd: Some("rust-service".to_string()),
                install: None,
                run: "cargo build --release".to_string(),
                exclude: Vec::new(),
            },
            crate::config::BuildStage {
                name: Some("frontend".to_string()),
                cwd: Some("apps/web".to_string()),
                install: None,
                run: "bun run build".to_string(),
                exclude: vec!["**/*.map".to_string()],
            },
        ];
        let includes = build_artifact_include_patterns(&config);
        assert_eq!(includes, vec!["**/*".to_string()]);
    }

    #[test]
    fn build_artifact_exclude_patterns_collects_from_stages() {
        let mut config = TakoToml::default();
        config.build_stages = vec![
            crate::config::BuildStage {
                name: None,
                cwd: Some("apps/web".to_string()),
                install: None,
                run: "bun run build".to_string(),
                exclude: vec!["**/*.map".to_string()],
            },
            crate::config::BuildStage {
                name: None,
                cwd: None,
                install: None,
                run: "bun run build".to_string(),
                exclude: vec!["tmp/**".to_string()],
            },
        ];
        let excludes = build_artifact_exclude_patterns(&BuildPreset::default(), &config);
        assert_eq!(
            excludes,
            vec!["apps/web/**/*.map".to_string(), "tmp/**".to_string(),]
        );
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
    fn summarize_build_stages_lists_custom_stages() {
        let custom = vec![
            crate::config::BuildStage {
                name: None,
                cwd: None,
                install: None,
                run: "bun run build".to_string(),
                exclude: Vec::new(),
            },
            crate::config::BuildStage {
                name: Some("frontend-assets".to_string()),
                cwd: Some("frontend".to_string()),
                install: None,
                run: "bun run build".to_string(),
                exclude: Vec::new(),
            },
        ];
        assert_eq!(
            summarize_build_stages(&custom),
            vec!["Stage 1".to_string(), "Stage 'frontend-assets'".to_string(),]
        );
    }

    #[test]
    fn run_local_build_executes_custom_stages_in_order() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(workspace.join("frontend")).unwrap();
        let stages = vec![
            crate::config::BuildStage {
                name: None,
                cwd: None,
                install: None,
                run: "printf 'stage-1-run\\n' >> \"$TAKO_APP_DIR/order.log\"".to_string(),
                exclude: Vec::new(),
            },
            crate::config::BuildStage {
                name: Some("frontend-assets".to_string()),
                cwd: Some("frontend".to_string()),
                install: Some(
                    "printf 'stage-2-install\\n' >> \"$TAKO_APP_DIR/order.log\"".to_string(),
                ),
                run: "printf 'stage-2-run\\n' >> \"$TAKO_APP_DIR/order.log\"".to_string(),
                exclude: Vec::new(),
            },
        ];

        run_local_build(
            &workspace,
            &workspace,
            &workspace,
            &workspace,
            &Default::default(),
            &stages,
            &[],
        )
        .unwrap();
        let order = std::fs::read_to_string(workspace.join("order.log")).unwrap();
        assert_eq!(order, "stage-1-run\nstage-2-install\nstage-2-run\n");
    }

    #[test]
    fn run_local_build_errors_when_stage_working_dir_is_missing() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let stages = vec![crate::config::BuildStage {
            name: None,
            cwd: Some("frontend".to_string()),
            install: None,
            run: "true".to_string(),
            exclude: Vec::new(),
        }];

        let err = run_local_build(
            &workspace,
            &workspace,
            &workspace,
            &workspace,
            &Default::default(),
            &stages,
            &[],
        )
        .unwrap_err();
        assert!(err.contains("Stage 1"));
        assert!(err.contains("working directory"));
    }

    #[test]
    fn run_local_build_defaults_cwd_to_app_dir() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let app_dir = workspace.join("apps/myapp");
        std::fs::create_dir_all(&app_dir).unwrap();
        let build_config = crate::config::BuildConfig {
            run: Some("touch marker.txt".to_string()),
            ..Default::default()
        };
        run_local_build(
            &workspace,
            &app_dir,
            &workspace,
            &app_dir,
            &build_config,
            &[],
            &[],
        )
        .unwrap();
        // marker.txt should be created in app_dir, not workspace root
        assert!(app_dir.join("marker.txt").exists());
        assert!(!workspace.join("marker.txt").exists());
    }

    #[test]
    fn run_local_build_stage_cwd_relative_to_app_dir() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let app_dir = workspace.join("apps/myapp");
        let sdk_dir = workspace.join("sdk");
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::create_dir_all(&sdk_dir).unwrap();
        let stages = vec![crate::config::BuildStage {
            name: Some("sdk".to_string()),
            cwd: Some("../../sdk".to_string()),
            install: None,
            run: "touch built.txt".to_string(),
            exclude: Vec::new(),
        }];
        run_local_build(
            &workspace,
            &app_dir,
            &workspace,
            &app_dir,
            &Default::default(),
            &stages,
            &[],
        )
        .unwrap();
        assert!(sdk_dir.join("built.txt").exists());
    }

    #[test]
    fn run_local_build_stage_cwd_rejects_workspace_escape() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        let app_dir = workspace.join("apps/myapp");
        std::fs::create_dir_all(&app_dir).unwrap();
        let stages = vec![crate::config::BuildStage {
            name: None,
            cwd: Some("../../../outside".to_string()),
            install: None,
            run: "true".to_string(),
            exclude: Vec::new(),
        }];
        let err = run_local_build(
            &workspace,
            &app_dir,
            &workspace,
            &app_dir,
            &Default::default(),
            &stages,
            &[],
        )
        .unwrap_err();
        assert!(err.contains("must not escape the project root"));
    }

    #[test]
    fn merge_assets_locally_merges_into_public_and_overwrites_last_write() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(workspace.join("dist/client")).unwrap();
        std::fs::create_dir_all(workspace.join("assets/shared")).unwrap();
        std::fs::write(workspace.join("dist/client/logo.txt"), "dist").unwrap();
        std::fs::write(workspace.join("assets/shared/logo.txt"), "shared").unwrap();

        merge_assets_locally(
            &workspace,
            &["dist/client".to_string(), "assets/shared".to_string()],
        )
        .unwrap();

        let merged = std::fs::read_to_string(workspace.join("public/logo.txt")).unwrap();
        assert_eq!(merged, "shared");
    }

    #[test]
    fn merge_assets_locally_fails_when_asset_root_is_missing() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();

        let err = merge_assets_locally(&workspace, &["missing".to_string()]).unwrap_err();
        assert!(err.contains("not found after build"));
    }

    #[test]
    fn restore_local_build_caches_copies_workspace_and_app_scoped_directories() {
        let temp = TempDir::new().unwrap();
        let cache_root = temp.path().join("cache");
        let workspace = temp.path().join("workspace");
        let app_dir = workspace.join("apps/web");

        std::fs::create_dir_all(cache_root.join(".turbo")).unwrap();
        std::fs::create_dir_all(cache_root.join(".next/cache")).unwrap();
        std::fs::create_dir_all(&app_dir).unwrap();
        std::fs::write(cache_root.join(".turbo/state.json"), "workspace-cache").unwrap();
        std::fs::write(cache_root.join(".next/cache/fetch-cache"), "app-cache").unwrap();

        let restored =
            restore_local_build_caches(&cache_root, &workspace, &app_dir, BuildAdapter::Node)
                .unwrap();

        assert_eq!(restored, 2);
        assert_eq!(
            std::fs::read_to_string(workspace.join(".turbo/state.json")).unwrap(),
            "workspace-cache"
        );
        assert_eq!(
            std::fs::read_to_string(app_dir.join(".next/cache/fetch-cache")).unwrap(),
            "app-cache"
        );
    }

    #[test]
    fn persist_local_build_caches_overwrites_stale_entries() {
        let temp = TempDir::new().unwrap();
        let cache_root = temp.path().join("cache");
        let workspace = temp.path().join("workspace");
        let app_dir = workspace.join("apps/web");

        std::fs::create_dir_all(cache_root.join(".turbo")).unwrap();
        std::fs::create_dir_all(cache_root.join(".next/cache")).unwrap();
        std::fs::create_dir_all(workspace.join(".turbo")).unwrap();
        std::fs::create_dir_all(app_dir.join(".next/cache")).unwrap();
        std::fs::write(cache_root.join(".turbo/stale.txt"), "stale").unwrap();
        std::fs::write(cache_root.join(".next/cache/stale.txt"), "stale").unwrap();
        std::fs::write(workspace.join(".turbo/state.json"), "fresh-workspace").unwrap();
        std::fs::write(app_dir.join(".next/cache/fetch-cache"), "fresh-app").unwrap();

        let persisted =
            persist_local_build_caches(&cache_root, &workspace, &app_dir, BuildAdapter::Node)
                .unwrap();

        assert_eq!(persisted, 2);
        assert_eq!(
            std::fs::read_to_string(cache_root.join(".turbo/state.json")).unwrap(),
            "fresh-workspace"
        );
        assert_eq!(
            std::fs::read_to_string(cache_root.join(".next/cache/fetch-cache")).unwrap(),
            "fresh-app"
        );
        assert!(!cache_root.join(".turbo/stale.txt").exists());
        assert!(!cache_root.join(".next/cache/stale.txt").exists());
    }

    #[test]
    fn local_build_cache_root_sanitizes_target_labels() {
        let temp = TempDir::new().unwrap();
        let root = local_build_cache_root(temp.path(), "linux/arm64 (shared)");
        assert!(root.ends_with(".tako/tmp/build-caches/linux_arm64__shared_"));
    }

    #[test]
    fn save_runtime_version_to_manifest_writes_version_to_app_json() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            workspace.join("app.json"),
            r#"{"runtime":"bun","main":"index.ts","idle_timeout":300}"#,
        )
        .unwrap();

        save_runtime_version_to_manifest(&workspace, "1.3.9").unwrap();

        let manifest_raw = std::fs::read_to_string(workspace.join("app.json")).unwrap();
        let manifest: serde_json::Value = serde_json::from_str(&manifest_raw).unwrap();
        assert_eq!(manifest["runtime_version"], "1.3.9");
        assert_eq!(manifest["runtime"], "bun");
    }

    #[test]
    fn extract_semver_from_version_output_handles_common_formats() {
        assert_eq!(
            extract_semver_from_version_output("bun 1.3.11"),
            Some("1.3.11".to_string())
        );
        assert_eq!(
            extract_semver_from_version_output(
                "deno 2.7.6 (stable, release, aarch64-apple-darwin)"
            ),
            Some("2.7.6".to_string())
        );
        assert_eq!(
            extract_semver_from_version_output("v22.12.0"),
            Some("22.12.0".to_string())
        );
    }

    #[test]
    fn save_runtime_version_cleans_up_old_version_file() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            workspace.join("app.json"),
            r#"{"runtime":"bun","main":"index.ts","idle_timeout":300}"#,
        )
        .unwrap();
        std::fs::write(workspace.join(RUNTIME_VERSION_OUTPUT_FILE), "1.3.9").unwrap();

        save_runtime_version_to_manifest(&workspace, "1.3.9").unwrap();

        assert!(!workspace.join(RUNTIME_VERSION_OUTPUT_FILE).exists());
    }

    #[test]
    fn resolve_runtime_version_from_workspace_ignores_old_runtime_version_file() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        let old_tools_file = format!(".{}{}", "proto", "tools");
        std::fs::write(workspace.join(old_tools_file), "bun = \"1.3.9\"\n").unwrap();

        let resolved = resolve_runtime_version_from_workspace(&workspace, "bun")
            .expect("resolve runtime version");

        assert_eq!(resolved, "latest");
    }

    #[test]
    fn package_target_artifact_packages_workspace_root_contents() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("index.ts"), "console.log('ok');").unwrap();
        std::fs::write(workspace.join("app.json"), r#"{"main":"index.ts"}"#).unwrap();

        let cache_dir = temp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let cache_paths = artifact_cache_paths(&cache_dir, "v1", Some("linux-aarch64-musl"));
        let archive_size = package_target_artifact(
            &workspace,
            &workspace,
            &[],
            &["**/*".to_string()],
            &[],
            &cache_paths,
            "linux-aarch64-musl",
        )
        .unwrap();
        assert!(archive_size > 0);

        let unpacked = temp.path().join("unpacked");
        BuildExecutor::extract_archive(&cache_paths.artifact_path, &unpacked).unwrap();

        assert!(unpacked.join("index.ts").exists());
        assert!(unpacked.join("app.json").exists());
    }

    #[test]
    fn package_target_artifact_for_bun_does_not_require_entrypoint_sources() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(workspace.join("index.ts"), "console.log('ok');").unwrap();
        std::fs::write(workspace.join("app.json"), r#"{"main":"index.ts"}"#).unwrap();

        let cache_dir = temp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let cache_paths = artifact_cache_paths(&cache_dir, "v1", Some("linux-aarch64-musl"));
        let archive_size = package_target_artifact(
            &workspace,
            &workspace,
            &[],
            &["**/*".to_string()],
            &[],
            &cache_paths,
            "linux-aarch64-musl",
        )
        .unwrap();
        assert!(archive_size > 0);
    }

    #[test]
    fn package_target_artifact_preserves_workspace_protocol_dependencies() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(workspace.join("src")).unwrap();
        std::fs::write(
            workspace.join("package.json"),
            r#"{"name":"web","dependencies":{"tako.sh":"workspace:*"}}"#,
        )
        .unwrap();
        std::fs::write(workspace.join("src/app.ts"), "export default {};\n").unwrap();

        let cache_dir = temp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let cache_paths = artifact_cache_paths(&cache_dir, "v1", Some("linux-aarch64-musl"));
        let archive_size = package_target_artifact(
            &workspace,
            &workspace,
            &[],
            &["**/*".to_string()],
            &["**/node_modules/**".to_string()],
            &cache_paths,
            "linux-aarch64-musl",
        )
        .unwrap();
        assert!(archive_size > 0);

        let unpacked = temp.path().join("unpacked");
        BuildExecutor::extract_archive(&cache_paths.artifact_path, &unpacked).unwrap();
        let package_json = std::fs::read_to_string(unpacked.join("package.json")).unwrap();
        let package_json: serde_json::Value = serde_json::from_str(&package_json).unwrap();
        assert_eq!(
            package_json
                .get("dependencies")
                .and_then(|deps| deps.get("tako.sh"))
                .and_then(|value| value.as_str()),
            Some("workspace:*")
        );
    }

    #[test]
    fn package_target_artifact_does_not_validate_workspace_protocol_dependencies() {
        let temp = TempDir::new().unwrap();
        let workspace = temp.path().join("workspace");
        std::fs::create_dir_all(&workspace).unwrap();
        std::fs::write(
            workspace.join("package.json"),
            r#"{"name":"web","dependencies":{"missing-pkg":"workspace:*"}}"#,
        )
        .unwrap();
        std::fs::write(workspace.join("src.ts"), "export default {};\n").unwrap();

        let cache_dir = temp.path().join("cache");
        std::fs::create_dir_all(&cache_dir).unwrap();
        let cache_paths = artifact_cache_paths(&cache_dir, "v1", Some("linux-aarch64-musl"));
        let archive_size = package_target_artifact(
            &workspace,
            &workspace,
            &[],
            &["**/*".to_string()],
            &[],
            &cache_paths,
            "linux-aarch64-musl",
        )
        .unwrap();
        assert!(archive_size > 0);
    }

    #[test]
    fn build_stage_summary_output_is_hidden_when_empty() {
        let summary: Vec<String> = vec![];
        assert_eq!(format_build_stages_summary_for_output(&summary, None), None);
    }

    #[test]
    fn build_stage_summary_output_is_shown_when_non_empty() {
        let summary = vec!["Stage 'preset'".to_string(), "Stage 2".to_string()];
        assert_eq!(
            format_build_stages_summary_for_output(&summary, Some("linux-x86_64-glibc")),
            Some("Build stages for linux-x86_64-glibc: Stage 'preset' -> Stage 2".to_string())
        );
    }
}
