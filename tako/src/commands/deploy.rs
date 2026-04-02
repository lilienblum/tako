use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::app::require_app_name_from_config_path;
use crate::build::{
    BuildAdapter, BuildCache, BuildError, BuildExecutor, BuildPreset, PresetGroup,
    apply_adapter_base_runtime_defaults, compute_file_hash, infer_adapter_from_preset_reference,
    js, load_build_preset, qualify_runtime_local_preset_ref,
};
use crate::commands::project_context;
use crate::commands::server;
use crate::config::{BuildStage, SecretsStore, ServerEntry, ServerTarget, ServersToml, TakoToml};
use crate::output;
use crate::ssh::{SshClient, SshConfig};
#[cfg(test)]
use crate::ui;
use crate::ui::{
    SUMMARY_INDENT, TaskItemState, TaskState, TaskTreeSession, TreeNode, TreeTextTone,
};
use crate::validation::{
    validate_full_config, validate_no_route_conflicts, validate_secrets_for_deployment,
};
use tako_core::{Command, Response};
use tracing::Instrument;

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
    main: String,
    use_unified_target_process: bool,
}

#[derive(Clone)]
struct ServerDeployTarget {
    name: String,
    server: ServerEntry,
    target_label: String,
    archive_path: PathBuf,
}

struct ServerCheck {
    name: String,
    mode: tako_core::UpgradeMode,
    dns_provider: Option<String>,
}

struct PreflightPhaseResult {
    checks: Vec<ServerCheck>,
    /// Pre-established SSH connections, keyed by server name.
    /// Kept alive from preflight so deploy can reuse them without reconnecting.
    ssh_clients: HashMap<String, SshClient>,
    elapsed: Duration,
}

struct BuildPhaseResult {
    version: String,
    manifest_main: String,
    deploy_secrets: HashMap<String, String>,
    use_unified_target_process: bool,
    artifacts_by_target: HashMap<String, PathBuf>,
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
    package_manager: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    commit_message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    git_dirty: Option<bool>,
    /// Path from the archive root to the app directory (where tako.toml lives).
    /// Empty string means the app is at the archive root (single-app projects).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    app_dir: String,
    /// Path from the archive root to the directory where deps should be installed.
    /// This is the runtime project root (where the lockfile lives).
    /// Empty string means install at the archive root.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    install_dir: String,
}

struct ValidationResult {
    tako_config: TakoToml,
    servers: ServersToml,
    secrets: SecretsStore,
    env: String,
    warnings: Vec<String>,
}

const ARTIFACT_CACHE_SCHEMA_VERSION: u32 = 0;
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

#[derive(Debug, Clone)]
struct DeployTaskTreeState {
    builds: Vec<TaskItemState>,
    deploys: Vec<TaskItemState>,
    success_lines: Vec<SummaryLine>,
    summary_line: Option<(String, TreeTextTone)>,
}

#[derive(Clone)]
struct DeployTaskTreeController {
    state: Arc<Mutex<DeployTaskTreeState>>,
    session: Option<TaskTreeSession>,
}

#[derive(Clone, Copy)]
enum DeployCompletionKind {
    Succeeded,
    Cancelled,
    Failed,
}

impl DeployTaskTreeController {
    fn new(server_names: &[String], build_groups: &[ArtifactBuildGroup]) -> Self {
        let state = DeployTaskTreeState {
            builds: build_groups
                .iter()
                .map(|group| {
                    let label = format_build_plan_target_label(group);
                    TaskItemState::pending(build_target_task_id(&label), label.clone())
                        .with_children(vec![
                            TaskItemState::pending(
                                build_task_step_id(&label, "probe-runtime"),
                                "Probe runtime",
                            ),
                            TaskItemState::pending(
                                build_task_step_id(&label, "build-artifact"),
                                "Build artifact",
                            ),
                            TaskItemState::pending(
                                build_task_step_id(&label, "package-artifact"),
                                "Package artifact",
                            ),
                        ])
                })
                .collect(),
            deploys: server_names
                .iter()
                .map(|server_name| {
                    TaskItemState::pending(deploy_target_task_id(server_name), server_name.clone())
                        .with_children(vec![
                            TaskItemState::pending(
                                deploy_task_step_id(server_name, "connecting"),
                                "Preflight",
                            ),
                            TaskItemState::pending(
                                deploy_task_step_id(server_name, "uploading"),
                                "Uploading",
                            ),
                            TaskItemState::pending(
                                deploy_task_step_id(server_name, "preparing"),
                                "Preparing",
                            ),
                            TaskItemState::pending(
                                deploy_task_step_id(server_name, "starting"),
                                "Starting",
                            ),
                        ])
                })
                .collect(),
            success_lines: Vec::new(),
            summary_line: None,
        };
        let tree = build_deploy_tree(&state);
        let session = should_use_deploy_task_tree().then(|| TaskTreeSession::new(tree));
        Self {
            state: Arc::new(Mutex::new(state)),
            session,
        }
    }

    fn fail_preflight_check(&self, server_name: &str, detail: impl Into<String>) {
        let msg = detail.into();
        self.fail_deploy_step(server_name, "connecting", msg.clone());
        self.rename_deploy_step(server_name, "connecting", "Preflight failed");
        self.fail_deploy_target_without_detail(server_name);
        self.warn_pending_deploy_children(server_name, "skipped");
    }

    fn mark_build_step_running(&self, target_label: &str, step: &str) {
        self.mark_running_by_id(&build_target_task_id(target_label));
        self.mark_running_by_id(&build_task_step_id(target_label, step));
    }

    fn succeed_build_step(&self, target_label: &str, step: &str, detail: Option<String>) {
        self.complete_by_id(
            &build_task_step_id(target_label, step),
            detail,
            DeployCompletionKind::Succeeded,
        );
    }

    fn warn_build_step(&self, target_label: &str, step: &str, detail: impl Into<String>) {
        self.complete_by_id(
            &build_task_step_id(target_label, step),
            Some(detail.into()),
            DeployCompletionKind::Cancelled,
        );
    }

    fn fail_build_step(&self, target_label: &str, step: &str, detail: impl Into<String>) {
        self.complete_by_id(
            &build_task_step_id(target_label, step),
            Some(detail.into()),
            DeployCompletionKind::Failed,
        );
    }

    fn append_cached_artifact_step(&self, target_label: &str, detail: Option<String>) {
        let parent_id = build_target_task_id(target_label);
        let child_id = build_task_step_id(target_label, "use-cached-artifact");
        let mut state = self.state.lock().unwrap();
        let parent = find_task_mut(&mut state.builds, &parent_id)
            .unwrap_or_else(|| panic!("missing build task {parent_id}"));
        if parent.find(&child_id).is_none() {
            let mut child = TaskItemState::pending(child_id.clone(), "Use cached artifact");
            if let Some(detail) = &detail {
                child = child.with_detail(detail.clone());
            }
            parent.append_child(child);
        }
        self.refresh_locked(&state);
        drop(state);
        self.succeed_build_step(target_label, "use-cached-artifact", detail);
    }

    fn succeed_build_target(&self, target_label: &str, detail: Option<String>) {
        self.complete_by_id(
            &build_target_task_id(target_label),
            detail,
            DeployCompletionKind::Succeeded,
        );
    }

    fn mark_build_target_running(&self, target_label: &str) {
        self.mark_running_by_id(&build_target_task_id(target_label));
    }

    fn fail_build_target(&self, target_label: &str, detail: impl Into<String>) {
        self.complete_by_id(
            &build_target_task_id(target_label),
            Some(detail.into()),
            DeployCompletionKind::Failed,
        );
    }

    fn warn_pending_build_children(&self, target_label: &str, reason: &str) {
        self.warn_pending_children(&build_target_task_id(target_label), reason);
    }

    fn mark_deploy_step_running(&self, server_name: &str, step: &str) {
        self.mark_running_by_id(&deploy_target_task_id(server_name));
        self.mark_running_by_id(&deploy_task_step_id(server_name, step));
    }

    fn update_deploy_step_progress(
        &self,
        server_name: &str,
        step: &str,
        detail: String,
        progress: f64,
    ) {
        self.update_by_id(&deploy_task_step_id(server_name, step), |task| {
            task.detail = Some(detail);
            task.progress = Some(progress);
        });
    }

    fn succeed_deploy_step(&self, server_name: &str, step: &str, detail: Option<String>) {
        self.complete_by_id(
            &deploy_task_step_id(server_name, step),
            detail,
            DeployCompletionKind::Succeeded,
        );
    }

    fn warn_deploy_step(&self, server_name: &str, step: &str, detail: impl Into<String>) {
        self.complete_by_id(
            &deploy_task_step_id(server_name, step),
            Some(detail.into()),
            DeployCompletionKind::Cancelled,
        );
    }

    fn fail_deploy_step(&self, server_name: &str, step: &str, detail: impl Into<String>) {
        self.complete_by_id(
            &deploy_task_step_id(server_name, step),
            Some(detail.into()),
            DeployCompletionKind::Failed,
        );
    }

    fn rename_deploy_step(&self, server_name: &str, step: &str, new_label: &str) {
        self.update_by_id(&deploy_task_step_id(server_name, step), |task| {
            task.label = new_label.to_string();
        });
    }

    fn succeed_deploy_target(&self, server_name: &str, detail: Option<String>) {
        self.complete_by_id(
            &deploy_target_task_id(server_name),
            detail,
            DeployCompletionKind::Succeeded,
        );
    }

    fn fail_deploy_target_without_detail(&self, server_name: &str) {
        self.complete_by_id(
            &deploy_target_task_id(server_name),
            None,
            DeployCompletionKind::Failed,
        );
    }

    fn warn_pending_deploy_children(&self, server_name: &str, reason: &str) {
        self.warn_pending_children(&deploy_target_task_id(server_name), reason);
    }

    fn abort_incomplete(&self, reason: &str) {
        let mut state = self.state.lock().unwrap();
        abort_incomplete_tasks(&mut state.builds, reason);
        abort_incomplete_tasks(&mut state.deploys, reason);
        self.refresh_locked(&state);
    }

    fn set_success_summary(&self, version: &str, routes: &[String]) {
        let mut state = self.state.lock().unwrap();
        state.success_lines = format_deploy_summary_lines("Release", version, routes);
        self.refresh_locked(&state);
    }

    fn set_error_summary(&self, summary: String) {
        let mut state = self.state.lock().unwrap();
        state.summary_line = Some((summary, TreeTextTone::Error));
        self.refresh_locked(&state);
    }

    fn finalize(&self) {
        if let Some(session) = &self.session {
            session.finalize();
        }
    }

    #[cfg(test)]
    fn snapshot(&self) -> DeployTaskTreeState {
        self.state.lock().unwrap().clone()
    }

    fn mark_running_by_id(&self, id: &str) {
        self.update_by_id(id, |task| {
            if matches!(task.state, TaskState::Pending) {
                task.state = TaskState::Running {
                    started_at: Instant::now(),
                };
                task.detail = None;
            }
        });
    }

    fn complete_by_id(&self, id: &str, detail: Option<String>, kind: DeployCompletionKind) {
        self.update_by_id(id, |task| {
            let elapsed = match task.state {
                TaskState::Running { started_at } => Some(started_at.elapsed()),
                _ => None,
            };
            task.state = match kind {
                DeployCompletionKind::Succeeded => TaskState::Succeeded { elapsed },
                DeployCompletionKind::Cancelled => TaskState::Cancelled { elapsed },
                DeployCompletionKind::Failed => TaskState::Failed { elapsed },
            };
            task.detail = detail;
            task.progress = None;
        });
    }

    fn warn_pending_children(&self, parent_id: &str, reason: &str) {
        let mut state = self.state.lock().unwrap();
        let parent = find_task_mut_in_state(&mut state, parent_id)
            .unwrap_or_else(|| panic!("missing parent task {parent_id}"));
        warn_pending_children(parent, reason);
        self.refresh_locked(&state);
    }

    fn update_by_id<F>(&self, id: &str, update: F)
    where
        F: FnOnce(&mut TaskItemState),
    {
        let mut state = self.state.lock().unwrap();
        let task =
            find_task_mut_in_state(&mut state, id).unwrap_or_else(|| panic!("missing task {id}"));
        update(task);
        self.refresh_locked(&state);
    }

    fn refresh_locked(&self, state: &DeployTaskTreeState) {
        if let Some(session) = &self.session {
            session.set_tree(build_deploy_tree(state));
        }
    }
}

fn should_use_deploy_task_tree() -> bool {
    output::is_pretty() && output::is_interactive()
}

fn abort_incomplete_tasks(tasks: &mut [TaskItemState], reason: &str) {
    for task in tasks {
        abort_incomplete_task(task, reason);
    }
}

fn abort_incomplete_task(task: &mut TaskItemState, reason: &str) {
    for child in &mut task.children {
        abort_incomplete_task(child, reason);
    }

    let elapsed = match task.state {
        TaskState::Running { started_at } => Some(started_at.elapsed()),
        _ => None,
    };

    match task.state {
        TaskState::Pending | TaskState::Running { .. } => {
            task.state = TaskState::Cancelled { elapsed };
        }
        TaskState::Succeeded { .. } | TaskState::Failed { .. } | TaskState::Cancelled { .. } => {}
    }
}

fn build_target_task_id(target_label: &str) -> String {
    format!("build:{target_label}")
}

fn build_task_step_id(target_label: &str, step: &str) -> String {
    format!("build:{target_label}:{step}")
}

fn deploy_target_task_id(server_name: &str) -> String {
    format!("deploy:{server_name}")
}

fn deploy_task_step_id(server_name: &str, step: &str) -> String {
    format!("deploy:{server_name}:{step}")
}

/// Build the render tree from deploy state. This replaces the old UiNode-based
/// `build_deploy_task_tree_root`. Controllers call this via `refresh_locked()`.
fn build_deploy_tree(state: &DeployTaskTreeState) -> Vec<TreeNode> {
    let mut tree = Vec::new();
    let has_deploys = !state.deploys.is_empty();

    // Build reporter
    match state.builds.as_slice() {
        [] => {}
        [build] => {
            let label = if matches!(build.state, TaskState::Succeeded { .. }) {
                "Built"
            } else {
                "Building"
            };
            tree.push(TreeNode::AccentTask(TaskItemState {
                id: build.id.clone(),
                label: label.to_string(),
                state: build.state.clone(),
                detail: build.detail.clone(),
                progress: None,
                children: vec![],
            }));
            if has_deploys {
                tree.push(TreeNode::Spacer);
            }
        }
        builds => {
            let group_state = aggregate_group_state(builds);
            tree.push(TreeNode::Task(TaskItemState {
                id: "build-group".into(),
                label: "Building".into(),
                state: group_state,
                detail: None,
                progress: None,
                children: builds
                    .iter()
                    .map(|b| TaskItemState {
                        id: b.id.clone(),
                        label: b.label.clone(),
                        state: b.state.clone(),
                        detail: b.detail.clone(),
                        progress: None,
                        children: vec![],
                    })
                    .collect(),
            }));
            if has_deploys {
                tree.push(TreeNode::Spacer);
            }
        }
    }

    // Deploy groups
    for (index, deploy) in state.deploys.iter().enumerate() {
        let label = match &deploy.state {
            TaskState::Succeeded { .. } => format!("Deployed to {}", deploy.label),
            TaskState::Failed { .. } => format!("Deploy to {} failed", deploy.label),
            _ => format!("Deploying to {}", deploy.label),
        };
        tree.push(TreeNode::Task(TaskItemState {
            id: deploy.id.clone(),
            label,
            state: deploy.state.clone(),
            detail: deploy.detail.clone(),
            progress: None,
            children: deploy.children.clone(),
        }));
        if index + 1 < state.deploys.len() {
            tree.push(TreeNode::Spacer);
        }
    }

    if !state.success_lines.is_empty() {
        if !tree.is_empty() && !matches!(tree.last(), Some(TreeNode::Spacer)) {
            tree.push(TreeNode::Spacer);
        }
        let max_label_width = state
            .success_lines
            .iter()
            .map(|l| l.label.len())
            .max()
            .unwrap_or(0);
        for line in &state.success_lines {
            let padded_label = if line.label.is_empty() {
                " ".repeat(max_label_width)
            } else {
                format!("{:<width$}", line.label, width = max_label_width)
            };
            tree.push(TreeNode::LabeledText {
                label: format!("{}{}", SUMMARY_INDENT, padded_label),
                value: line.value.clone(),
            });
        }
    }

    if let Some((summary, tone)) = &state.summary_line {
        if !tree.is_empty() && !matches!(tree.last(), Some(TreeNode::Spacer)) {
            tree.push(TreeNode::Spacer);
        }
        tree.push(TreeNode::Text {
            text: summary.clone(),
            tone: tone.clone(),
        });
    }

    tree
}

fn aggregate_group_state(tasks: &[TaskItemState]) -> TaskState {
    if let Some(started_at) = tasks.iter().find_map(|task| match &task.state {
        TaskState::Running { started_at } => Some(*started_at),
        _ => None,
    }) {
        return TaskState::Running { started_at };
    }

    if tasks
        .iter()
        .any(|task| matches!(task.state, TaskState::Failed { .. }))
    {
        return TaskState::Failed { elapsed: None };
    }

    let all_succeeded = tasks
        .iter()
        .all(|task| matches!(task.state, TaskState::Succeeded { .. }));

    if all_succeeded {
        TaskState::Succeeded { elapsed: None }
    } else {
        TaskState::Pending
    }
}

fn find_task_mut<'a>(tasks: &'a mut [TaskItemState], id: &str) -> Option<&'a mut TaskItemState> {
    tasks.iter_mut().find_map(|task| task.find_mut(id))
}

fn find_task_mut_in_state<'a>(
    state: &'a mut DeployTaskTreeState,
    id: &str,
) -> Option<&'a mut TaskItemState> {
    find_task_mut(&mut state.builds, id).or_else(|| find_task_mut(&mut state.deploys, id))
}

fn warn_pending_children(parent: &mut TaskItemState, reason: &str) {
    for child in &mut parent.children {
        if matches!(child.state, TaskState::Pending) {
            child.state = TaskState::Cancelled { elapsed: None };
            child.detail = Some(reason.to_string());
        }
        warn_pending_children(child, reason);
    }
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

pub fn run(
    env: Option<&str>,
    assume_yes: bool,
    config_path: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Use tokio runtime for async SSH operations
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(run_async(env, assume_yes, config_path))
}

async fn run_async(
    requested_env: Option<&str>,
    assume_yes: bool,
    config_path: Option<&Path>,
) -> Result<(), Box<dyn std::error::Error>> {
    let context = project_context::resolve_existing(config_path)?;
    let project_dir = context.project_dir;
    let validation = output::with_spinner_silent(
        "Validating configuration",
        || -> Result<ValidationResult, String> {
            let _t = output::timed("Configuration validation");
            let tako_config =
                TakoToml::load_from_file(&context.config_path).map_err(|e| e.to_string())?;
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

    let eff_app_dir = project_dir.clone();

    let preflight_preset_ref = resolve_build_preset_ref(&eff_app_dir, &tako_config)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let preflight_runtime_adapter =
        resolve_effective_build_adapter(&eff_app_dir, &tako_config, &preflight_preset_ref)
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    let source_root = source_bundle_root(&project_dir, preflight_runtime_adapter.id());

    if preflight_runtime_adapter.preset_group() == PresetGroup::Js {
        let _ = js::write_types(&project_dir);
    }

    let _bun_lockfile_checked = if should_run_bun_lockfile_preflight(preflight_runtime_adapter) {
        output::with_spinner_silent("Checking Bun lockfile", || {
            let _t = output::timed("Bun lockfile check");
            run_bun_lockfile_preflight(&source_root)
        })
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?
    } else {
        false
    };

    // Skip confirmation if the user explicitly passed --env production (they
    // already know which environment they're targeting).
    let env_was_explicit = requested_env.is_some();
    confirm_production_deploy(&env, assume_yes || env_was_explicit || output::is_dry_run())
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    for warning in &warnings {
        output::warning(&format!("Validation: {}", warning));
    }

    let app_name = require_app_name_from_config_path(&context.config_path).map_err(
        |e| -> Box<dyn std::error::Error> {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, e.to_string()).into()
        },
    )?;
    let routes = required_env_routes(&tako_config, &env)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;

    let server_names = if output::is_dry_run() {
        resolve_deploy_server_names(&tako_config, &servers, &env)
            .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?
    } else {
        resolve_deploy_server_names_with_setup(
            &tako_config,
            &mut servers,
            &env,
            &context.config_path,
        )
        .await
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?
    };

    for server_name in &server_names {
        if !servers.contains(server_name) {
            return Err(format_server_not_found_error(server_name).into());
        }
    }

    let server_targets = resolve_deploy_server_targets(&servers, &server_names)
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    tracing::debug!("{}", format_servers_summary(&server_names));

    let use_unified_js_target_process =
        should_use_unified_js_target_process(preflight_runtime_adapter.id());
    if let Some(server_targets_summary) =
        format_server_targets_summary(&server_targets, use_unified_js_target_process)
    {
        tracing::debug!("{}", server_targets_summary);
    }
    let build_groups = build_artifact_target_groups(&server_targets, use_unified_js_target_process);

    let deploy_task_tree = should_use_deploy_task_tree()
        .then(|| DeployTaskTreeController::new(&server_names, &build_groups));

    if let (Some(task_tree), Some(first_build_group)) = (&deploy_task_tree, build_groups.first()) {
        task_tree.mark_build_target_running(&format_build_plan_target_label(first_build_group));
    }

    if output::is_dry_run() {
        output::dry_run_skip("Server checks");
        output::dry_run_skip("Build");
        for name in &server_names {
            output::dry_run_skip(&format!("Deploy to {}", output::strong(name)));
        }
        if output::is_pretty() {
            eprintln!();
        }
        print_deploy_summary("App", &app_name, &routes);
        return Ok(());
    }

    let use_per_server_spinners = deploy_task_tree.is_none()
        && should_use_per_server_spinners(server_names.len(), output::is_interactive());

    let preflight_deploy_app_name = tako_core::deployment_app_id(&app_name, &env);
    let mut preflight_handle = tokio::spawn(run_server_preflight_checks(
        server_names.clone(),
        servers.clone(),
        preflight_deploy_app_name,
        routes.clone(),
        deploy_task_tree.clone(),
    ));
    let mut build_handle = tokio::spawn(prepare_build_phase(
        project_dir.clone(),
        source_root.clone(),
        eff_app_dir.clone(),
        app_name.clone(),
        env.clone(),
        tako_config.clone(),
        secrets.clone(),
        preflight_preset_ref.clone(),
        preflight_runtime_adapter,
        server_targets.clone(),
        build_groups.clone(),
        deploy_task_tree.clone(),
    ));

    let mut preflight_result: Option<Result<PreflightPhaseResult, String>> = None;
    let mut build_result: Option<BuildPhaseResult> = None;

    while preflight_result.is_none() || build_result.is_none() {
        tokio::select! {
            result = &mut preflight_handle, if preflight_result.is_none() => {
                let result = result
                    .map_err(|e| format!("Server checks task failed: {}", e))
                    .and_then(|result| result);
                match result {
                    Ok(preflight) => {
                        if deploy_task_tree.is_none() {
                            output::success_with_elapsed(
                                &format_preflight_complete_message(&server_names),
                                preflight.elapsed,
                            );
                        }
                        preflight_result = Some(Ok(preflight));
                    }
                    Err(error) => {
                        if let Some(task_tree) = &deploy_task_tree {
                            task_tree.abort_incomplete("Aborted");
                            if build_result.is_none() {
                                build_handle.abort();
                            }
                            return Err(output::silent_exit_error().into());
                        }
                        if build_result.is_none() {
                            build_handle.abort();
                        }
                        return Err(error.into());
                    }
                }
            }
            result = &mut build_handle, if build_result.is_none() => {
                let result = result
                    .map_err(|e| format!("Build task failed: {}", e))
                    .and_then(|result| result);
                match result {
                    Ok(build) => build_result = Some(build),
                    Err(error) => {
                        if let Some(task_tree) = &deploy_task_tree {
                            task_tree.abort_incomplete("Aborted");
                            if preflight_result.is_none() {
                                preflight_handle.abort();
                            }
                            return Err(output::silent_exit_error().into());
                        }
                        if preflight_result.is_none() {
                            preflight_handle.abort();
                        }
                        return Err(error.into());
                    }
                }
            }
        }
    }

    let preflight = preflight_result
        .unwrap()
        .map_err(|e| -> Box<dyn std::error::Error> { e.into() })?;
    let mut preflight_ssh_clients = preflight.ssh_clients;
    check_wildcard_dns_support(&routes, &preflight.checks)?;

    let BuildPhaseResult {
        version,
        manifest_main,
        deploy_secrets,
        use_unified_target_process: use_unified_js_target_process,
        artifacts_by_target,
    } = build_result.expect("build result should be present");

    // ===== Deploy =====

    let secrets_hash = tako_core::compute_secrets_hash(&deploy_secrets);
    let deployment_app_name = tako_core::deployment_app_id(&app_name, &env);
    let deploy_config = Arc::new(DeployConfig {
        app_name: deployment_app_name.clone(),
        version: version.clone(),
        remote_base: format!("/opt/tako/apps/{}", deployment_app_name),
        routes: routes.clone(),
        env_vars: deploy_secrets,
        secrets_hash,
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
        targets.push(ServerDeployTarget {
            name: server_name.clone(),
            server,
            target_label,
            archive_path: archive_path.clone(),
        });
    }
    if deploy_task_tree.is_none() && targets.len() > 1 {
        output::info(&format_parallel_deploy_step(targets.len()));
    }

    // Spawn parallel deploy tasks
    let mut handles = Vec::new();
    for target in &targets {
        let server = target.server.clone();
        let server_name = target.name.clone();
        let target_label = target.target_label.clone();
        let archive_path = target.archive_path.clone();
        let deploy_config = deploy_config.clone();
        let use_spinner = use_per_server_spinners;
        let task_tree = deploy_task_tree.clone();
        let preconnected_ssh = preflight_ssh_clients.remove(&server_name);
        let span = output::scope(&server_name);
        let handle = tokio::spawn(
            async move {
                let result = deploy_to_server(
                    &deploy_config,
                    &server_name,
                    &server,
                    &archive_path,
                    &target_label,
                    use_spinner,
                    task_tree,
                    preconnected_ssh,
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

    let deploy_results = if deploy_task_tree.is_none()
        && output::is_interactive()
        && !use_per_server_spinners
        && handles.len() > 1
    {
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
                    if deploy_task_tree.is_none() {
                        output::bullet(&format_server_deploy_success(&server_name, &server));
                    }
                }
                Err(e) => {
                    // When using per-server spinners (single interactive server), the step
                    // spinner already printed the error detail. Skip the duplicate.
                    if deploy_task_tree.is_none() && !use_per_server_spinners {
                        output::error(&format_server_deploy_failure(
                            &server_name,
                            &server,
                            &e.to_string(),
                        ));
                    }
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
        if let Some(task_tree) = &deploy_task_tree {
            task_tree.set_success_summary(&version, &routes);
            task_tree.finalize();
        } else {
            if output::is_pretty() {
                eprintln!();
            }
            print_deploy_summary("Release", &version, &routes);
        }

        Ok(())
    } else {
        let succeeded = targets.len() - errors.len();
        let total = targets.len();
        if output::is_pretty() {
            if let Some(task_tree) = &deploy_task_tree {
                task_tree.set_error_summary(format!("Deployed to {succeeded}/{total} servers"));
                task_tree.finalize();
            } else {
                eprintln!(
                    "{}",
                    output::theme_error(format!("{succeeded}/{total} servers deployed"))
                );
            }
            return Err(output::silent_exit_error().into());
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
    format!("Deploy to {} now?", output::strong("production"),)
}

fn format_production_deploy_confirm_hint() -> String {
    output::theme_muted("Pass --yes/-y to skip this prompt.")
}

fn confirm_production_deploy(env: &str, assume_yes: bool) -> std::io::Result<()> {
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
    .map_err(|e| std::io::Error::new(e.kind(), format!("Failed to read confirmation: {e}")))?;
    if confirmed {
        Ok(())
    } else {
        Err(output::operation_cancelled_error())
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
    config_path: &Path,
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
                servers.names()[0].to_string()
            } else {
                select_production_server_for_mapping(servers)?
            };

            persist_server_env_mapping(config_path, &selected_server, env)?;
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

#[cfg(test)]
fn format_deploy_overview_lines(
    app_name: &str,
    _env: &str,
    target_count: usize,
    primary_target_and_server: Option<(&str, &ServerEntry)>,
) -> Vec<String> {
    let mut lines = vec![format!("App       : {app_name}")];
    match primary_target_and_server {
        Some((target_name, server)) => {
            lines.push(format!("Target    : {target_name}"));
            lines.push(format!("Host      : tako@{}:{}", server.host, server.port));
        }
        None => {
            let label = if target_count == 1 {
                "1 server".to_string()
            } else {
                format!("{target_count} servers")
            };
            lines.push(format!("Target    : {label}"));
        }
    }
    lines
}

fn format_build_plan_target_label(group: &ArtifactBuildGroup) -> String {
    group
        .display_target_label
        .as_deref()
        .unwrap_or("shared target")
        .to_string()
}

fn format_preflight_complete_message(server_names: &[String]) -> String {
    if server_names.len() == 1 {
        format!("Checked {}", server_names[0])
    } else {
        format!("Checked {} servers", server_names.len())
    }
}

/// A label + value pair for the deploy summary. The label is rendered in accent
/// color and the value in the default (normal) color.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SummaryLine {
    label: String,
    value: String,
}

fn format_deploy_summary_lines(
    primary_label: &str,
    primary_value: &str,
    routes: &[String],
) -> Vec<SummaryLine> {
    let mut lines = vec![SummaryLine {
        label: primary_label.to_string(),
        value: primary_value.to_string(),
    }];
    if let Some((first_route, remaining_routes)) = routes.split_first() {
        lines.push(SummaryLine {
            label: "Routes".to_string(),
            value: format_route_url(first_route),
        });
        for route in remaining_routes {
            lines.push(SummaryLine {
                label: String::new(),
                value: format_route_url(route),
            });
        }
    }
    lines
}

fn print_deploy_summary(primary_label: &str, primary_value: &str, routes: &[String]) {
    let lines = format_deploy_summary_lines(primary_label, primary_value, routes);
    let max_label_width = lines.iter().map(|l| l.label.len()).max().unwrap_or(0);
    for line in lines {
        let padded_label = format!("{:<width$}", line.label, width = max_label_width);
        let formatted = format!("{} {}", padded_label, line.value);
        if output::is_pretty() {
            output::info(&formatted);
        } else {
            tracing::info!("{}", formatted);
        }
    }
}

fn format_route_url(route: &str) -> String {
    format!("https://{route}")
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
        Some(label) => format!("Built for {}", label),
        None => "Built".to_string(),
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

fn format_deploy_step_failure(step: &str, error: &str) -> String {
    format!("{step} failed: {error}")
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
    config_path: &Path,
    server_name: &str,
    env: &str,
) -> Result<(), String> {
    TakoToml::upsert_server_env_in_file(config_path, server_name, env).map_err(|e| {
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

fn source_bundle_root(project_dir: &Path, runtime_id: &str) -> PathBuf {
    match git_repo_root(project_dir) {
        Some(root) if project_dir.starts_with(&root) => root,
        _ => tako_runtime::find_runtime_project_root(runtime_id, project_dir),
    }
}

/// Compute the effective app directory on the local filesystem,
/// incorporating `app_dir` from config if set.
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

    // Try runtime entrypoint inference (candidate files like index.ts)
    if let Some(inferred) = runtime_adapter.infer_main_entrypoint(project_dir) {
        return Ok(inferred);
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
    package_manager: Option<String>,
    commit_message: Option<String>,
    git_dirty: Option<bool>,
    app_env_vars: HashMap<String, String>,
    runtime_env_vars: HashMap<String, String>,
    env_secrets: Option<&HashMap<String, String>>,
    app_dir: String,
    install_dir: String,
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
        package_manager,
        commit_message,
        git_dirty,
        app_dir,
        install_dir,
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
    runtime_env_vars: HashMap<String, String>,
    environment: &str,
    runtime_name: &str,
) -> BTreeMap<String, String> {
    let mut merged = BTreeMap::new();

    // 1. Runtime defaults for this environment (lowest priority)
    if let Some(def) = tako_runtime::runtime_def_for(runtime_name, None)
        && let Some(env_defaults) = def.envs.environments.get(environment)
    {
        for (key, value) in env_defaults {
            merged.insert(key.clone(), value.clone());
        }
    }

    // 2. App-level env vars from tako.toml [vars] + [vars.<env>]
    for (key, value) in app_env_vars {
        merged.insert(key, value);
    }

    // 3. Runtime env vars (from runtime detection)
    for (key, value) in runtime_env_vars {
        merged.insert(key, value);
    }

    // 4. TAKO_ENV always set (highest priority)
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

fn should_use_unified_js_target_process(runtime_tool: &str) -> bool {
    matches!(runtime_tool, "bun" | "node" | "deno")
}

fn shorten_commit(commit: &str) -> &str {
    &commit[..commit.len().min(12)]
}

async fn run_server_preflight_checks(
    server_names: Vec<String>,
    servers: ServersToml,
    deploy_app_name: String,
    routes: Vec<String>,
    task_tree: Option<DeployTaskTreeController>,
) -> Result<PreflightPhaseResult, String> {
    let start = Instant::now();
    let mut check_set = tokio::task::JoinSet::new();

    for server_name in &server_names {
        let server = servers
            .get(server_name)
            .ok_or_else(|| format!("Server '{}' not found in servers.toml", server_name))?;
        let name = server_name.clone();
        let task_tree_for_task = task_tree.clone();
        let check_name = name.clone();
        let ssh_config = SshConfig::from_server(&server.host, server.port);
        let check_deploy_app_name = deploy_app_name.clone();
        let check_routes = routes.clone();
        let span = output::scope(&name);
        check_set.spawn(
            async move {
                let result = async {
                    tracing::debug!("Preflight check…");
                    let _t = output::timed("Preflight check");

                    // Mark "Preflight" as running in the task tree — this runs
                    // concurrently with the build phase so the user sees progress.
                    if let Some(task_tree) = &task_tree_for_task {
                        task_tree.mark_deploy_step_running(&name, "connecting");
                    }

                    let mut ssh = SshClient::new(ssh_config);
                    ssh.connect().await?;
                    let info = ssh.tako_server_info().await?;

                    let mut mode = info.mode;

                    if mode == tako_core::UpgradeMode::Upgrading {
                        let reset_cmd = SshClient::run_with_root_or_sudo(
                            "sqlite3 /opt/tako/tako.db \
                         \"UPDATE server_state SET server_mode = 'normal' WHERE id = 1; \
                          DELETE FROM upgrade_lock WHERE id = 1;\"",
                        );
                        if ssh.exec_checked(&reset_cmd).await.is_ok() {
                            let _ = ssh.tako_restart().await;
                            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
                            if let Ok(new_info) = ssh.tako_server_info().await {
                                mode = new_info.mode;
                            }
                        }
                    }

                    // Disk space check
                    ensure_remote_disk_space(&ssh)
                        .await
                        .map_err(|e| crate::ssh::SshError::Connection(e.to_string()))?;

                    // Route conflict check
                    let existing = parse_existing_routes_response(ssh.tako_routes().await?)
                        .map_err(|e| crate::ssh::SshError::Connection(e.to_string()))?;
                    validate_no_route_conflicts(&existing, &check_deploy_app_name, &check_routes)
                        .map_err(|e| {
                        crate::ssh::SshError::Connection(format!("Route conflict: {}", e))
                    })?;

                    // Mark "Preflight" as succeeded — connection stays open for
                    // the deploy phase to reuse.
                    if let Some(task_tree) = &task_tree_for_task {
                        task_tree.succeed_deploy_step(&name, "connecting", None);
                    }

                    Ok::<_, crate::ssh::SshError>((
                        ServerCheck {
                            name,
                            mode,
                            dns_provider: info.dns_provider,
                        },
                        ssh,
                    ))
                }
                .await;
                if let Err(error) = &result
                    && let Some(task_tree) = &task_tree_for_task
                {
                    task_tree.fail_preflight_check(&check_name, error.to_string());
                }
                result
            }
            .instrument(span),
        );
    }

    let mut checks = Vec::new();
    let mut ssh_clients = HashMap::new();
    while let Some(result) = check_set.join_next().await {
        let (check, ssh) = result
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;

        if check.mode == tako_core::UpgradeMode::Upgrading {
            if let Some(task_tree) = &task_tree {
                task_tree.fail_preflight_check(&check.name, "Server is currently upgrading");
            }
            return Err(format!(
                "{} is currently upgrading. Retry after the upgrade completes.",
                check.name,
            ));
        }

        ssh_clients.insert(check.name.clone(), ssh);
        checks.push(check);
    }

    Ok(PreflightPhaseResult {
        checks,
        ssh_clients,
        elapsed: start.elapsed(),
    })
}

fn check_wildcard_dns_support(
    routes: &[String],
    checks: &[ServerCheck],
) -> Result<(), Box<dyn std::error::Error>> {
    let wildcard_routes: Vec<_> = routes.iter().filter(|r| r.starts_with("*.")).collect();
    if wildcard_routes.is_empty() {
        return Ok(());
    }

    if checks.iter().all(|c| c.dns_provider.is_some()) {
        tracing::debug!("All servers support wildcard domains");
        return Ok(());
    }

    let missing: Vec<_> = checks
        .iter()
        .filter(|c| c.dns_provider.is_none())
        .map(|c| c.name.as_str())
        .collect();
    let route_list = wildcard_routes
        .iter()
        .map(|r| r.as_str())
        .collect::<Vec<_>>()
        .join(", ");

    Err(format!(
        "Server(s) {} need DNS-01 for wildcard route(s) {route_list}\n\
         Run `tako servers setup-wildcard` to configure DNS credentials.",
        missing.join(", "),
    )
    .into())
}

async fn prepare_build_phase(
    project_dir: PathBuf,
    source_root: PathBuf,
    eff_app_dir: PathBuf,
    app_name: String,
    env: String,
    tako_config: TakoToml,
    secrets: SecretsStore,
    preset_ref: String,
    runtime_adapter: BuildAdapter,
    server_targets: Vec<(String, ServerTarget)>,
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
                load_build_preset(&eff_app_dir, &preset_ref),
            )
            .await
            .map_err(|e| e.to_string())?
        } else {
            load_build_preset(&eff_app_dir, &preset_ref)
                .await
                .map_err(|e| e.to_string())?
        }
    };
    tracing::debug!(
        "Resolved preset: {} (commit {})",
        resolved_preset.preset_ref,
        shorten_commit(&resolved_preset.commit)
    );

    let plugin_ctx = tako_runtime::PluginContext {
        project_dir: &eff_app_dir,
        package_manager: tako_config.package_manager.as_deref(),
    };
    apply_adapter_base_runtime_defaults(&mut build_preset, runtime_adapter, Some(&plugin_ctx))
        .map_err(|e| e.to_string())?;
    tracing::debug!(
        "Build preset: {} @ {}",
        resolved_preset.preset_ref,
        shorten_commit(&resolved_preset.commit)
    );
    tracing::debug!("{}", format_runtime_summary(&build_preset.name, None));
    let runtime_tool = runtime_adapter.id().to_string();

    let manifest_main = resolve_deploy_main(
        &eff_app_dir,
        runtime_adapter,
        &tako_config,
        build_preset.main.as_deref(),
    )?;
    tracing::debug!(
        "{}",
        format_entry_point_summary(&eff_app_dir.join(&manifest_main),)
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

    if let Some(server_targets_summary) = format_server_targets_summary(
        &server_targets,
        should_use_unified_js_target_process(&runtime_tool),
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
        use_unified_target_process: should_use_unified_js_target_process(&runtime_tool),
        artifacts_by_target,
    })
}

fn build_artifact_include_patterns(config: &TakoToml) -> Vec<String> {
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
fn should_report_artifact_include_patterns(include_patterns: &[String]) -> bool {
    if include_patterns.is_empty() {
        return false;
    }
    !(include_patterns.len() == 1 && include_patterns[0] == "**/*")
}

fn build_artifact_exclude_patterns(_preset: &BuildPreset, config: &TakoToml) -> Vec<String> {
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

fn build_asset_roots(preset: &BuildPreset, config: &TakoToml) -> Result<Vec<String>, String> {
    let mut merged = Vec::new();
    for root in preset.assets.iter().chain(config.assets.iter()) {
        let normalized = normalize_asset_root(root)?;
        if !merged.contains(&normalized) {
            merged.push(normalized);
        }
    }
    Ok(merged)
}

fn should_run_bun_lockfile_preflight(runtime_adapter: BuildAdapter) -> bool {
    runtime_adapter == BuildAdapter::Bun
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
        let build_target_label = unique_targets[0].clone();
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
                "Invalid runtime '{}'; expected one of: bun, node, deno, go",
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

fn run_local_build(
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

fn format_stage_label(stage_number: usize, stage_name: Option<&str>) -> String {
    match stage_name.map(str::trim).filter(|value| !value.is_empty()) {
        Some(name) => format!("Stage '{name}'"),
        None => format!("Stage {stage_number}"),
    }
}

fn summarize_build_stages(custom_stages: &[BuildStage]) -> Vec<String> {
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
fn save_runtime_version_to_manifest(workspace: &Path, runtime_version: &str) -> Result<(), String> {
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
fn extract_semver_from_version_output(output: &str) -> Option<String> {
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
        let command = format!("{} --version", shell_single_quote(runtime_tool));
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

fn merge_assets_locally(workspace_root: &Path, asset_roots: &[String]) -> Result<(), String> {
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

fn package_target_artifact(
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

fn extract_server_error_message(response: &str) -> String {
    serde_json::from_str::<serde_json::Value>(response)
        .ok()
        .and_then(|v| v["message"].as_str().map(String::from))
        .map(|message| {
            message
                .strip_prefix("Deploy failed: ")
                .unwrap_or(&message)
                .to_string()
        })
        .unwrap_or_else(|| response.to_string())
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
/// Fixed minimum free disk space required on the remote server.
const DEPLOY_MIN_FREE_DISK_BYTES: u64 = 256 * 1024 * 1024;

use crate::shell::shell_single_quote;

fn parse_df_available_kb(stdout: &str) -> Result<u64, String> {
    let line = stdout
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .ok_or_else(|| "empty df output".to_string())?;
    line.parse::<u64>()
        .map_err(|_| format!("unexpected df output: '{line}'"))
}

fn cleanup_partial_release_command(release_dir: &str) -> String {
    format!("rm -rf {}", shell_single_quote(release_dir))
}

async fn ensure_remote_disk_space(
    ssh: &SshClient,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
    if available_bytes < DEPLOY_MIN_FREE_DISK_BYTES {
        return Err(format!(
            "Insufficient disk space under {}. Required: at least {}. Available: {}.",
            DEPLOY_DISK_CHECK_PATH,
            format_size(DEPLOY_MIN_FREE_DISK_BYTES),
            format_size(available_bytes),
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

async fn connect_and_prepare_remote_release_dir(
    ssh: &mut SshClient,
    release_dir: &str,
    shared_dir: &str,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    ssh.connect().await?;
    prepare_remote_release_dir(ssh, release_dir, shared_dir).await
}

/// Prepare the remote release directory on an already-connected SSH session.
async fn prepare_remote_release_dir(
    ssh: &SshClient,
    release_dir: &str,
    shared_dir: &str,
) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
    let release_dir_preexisted = remote_directory_exists(ssh, release_dir).await?;
    if !release_dir_preexisted {
        ssh.exec_checked(&format!("mkdir -p {} {}", release_dir, shared_dir))
            .await?;
    }

    Ok(release_dir_preexisted)
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
        let error_label = format!("{} failed", loading.trim_end_matches('…'));
        output::with_spinner_async_err(loading, success, &error_label, work)
            .await
            .map_err(Into::into)
    } else {
        tracing::debug!("{}", loading);
        work.await.map_err(Into::into)
    }
}

async fn run_task_tree_deploy_step<T, E, Fut>(
    task_tree: &DeployTaskTreeController,
    server_name: &str,
    step: &str,
    work: Fut,
) -> Result<T, Box<dyn std::error::Error + Send + Sync>>
where
    Fut: Future<Output = Result<T, E>> + Send,
    T: Send,
    E: Send + std::fmt::Display + Into<Box<dyn std::error::Error + Send + Sync>>,
{
    run_task_tree_deploy_step_with_detail(task_tree, server_name, step, None, work).await
}

async fn run_task_tree_deploy_step_with_detail<T, E, Fut>(
    task_tree: &DeployTaskTreeController,
    server_name: &str,
    step: &str,
    success_detail: Option<String>,
    work: Fut,
) -> Result<T, Box<dyn std::error::Error + Send + Sync>>
where
    Fut: Future<Output = Result<T, E>> + Send,
    T: Send,
    E: Send + std::fmt::Display + Into<Box<dyn std::error::Error + Send + Sync>>,
{
    run_task_tree_deploy_step_with_detail_and_error_cleanup(
        task_tree,
        server_name,
        step,
        success_detail,
        work,
        || async {},
    )
    .await
}

async fn run_task_tree_deploy_step_with_detail_and_error_cleanup<T, E, Fut, Cleanup, CleanupFut>(
    task_tree: &DeployTaskTreeController,
    server_name: &str,
    step: &str,
    success_detail: Option<String>,
    work: Fut,
    cleanup_on_error: Cleanup,
) -> Result<T, Box<dyn std::error::Error + Send + Sync>>
where
    Fut: Future<Output = Result<T, E>> + Send,
    T: Send,
    E: Send + std::fmt::Display + Into<Box<dyn std::error::Error + Send + Sync>>,
    Cleanup: FnOnce() -> CleanupFut + Send,
    CleanupFut: Future<Output = ()> + Send,
{
    task_tree.mark_deploy_step_running(server_name, step);
    match work.await {
        Ok(value) => {
            let success_label = match step {
                "connecting" => "Preflight",
                "uploading" => "Uploaded",
                "preparing" => "Prepared",
                "starting" => "Started",
                _ => step,
            };
            task_tree.rename_deploy_step(server_name, step, success_label);
            task_tree.succeed_deploy_step(server_name, step, success_detail);
            Ok(value)
        }
        Err(error) => {
            let message = error.to_string();
            cleanup_on_error().await;
            task_tree.fail_deploy_step(server_name, step, message.clone());
            let failed_label = match step {
                "connecting" => "Preflight failed",
                "uploading" => "Upload failed",
                "preparing" => "Prepare failed",
                "starting" => "Start failed",
                _ => step,
            };
            task_tree.rename_deploy_step(server_name, step, failed_label);
            task_tree.fail_deploy_target_without_detail(server_name);
            task_tree.warn_pending_deploy_children(server_name, "skipped");
            Err(error.into())
        }
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

/// Deploy to a single server.
///
/// If `preconnected_ssh` is provided (from the preflight phase), the existing
/// connection is reused and the "Preflight" task-tree step is skipped (it was
/// already marked complete during preflight).  Otherwise a fresh SSH connection
/// is established here.
#[allow(clippy::too_many_arguments)]
async fn deploy_to_server(
    config: &DeployConfig,
    server_name: &str,
    server: &ServerEntry,
    archive_path: &Path,
    target_label: &str,
    use_spinner: bool,
    task_tree: Option<DeployTaskTreeController>,
    preconnected_ssh: Option<SshClient>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    tracing::debug!("Deploying (target: {target_label}, port: {})…", server.port);
    let _server_deploy_timer = output::timed("Server deploy");
    let release_dir = config.release_dir();

    let (mut ssh, release_dir_preexisted) = if let Some(ssh) = preconnected_ssh {
        // Reuse connection from preflight — "Preflight" is already done.
        // Just prepare the remote release directory.
        let preexisted = prepare_remote_release_dir(&ssh, &release_dir, &config.shared_dir())
            .await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e })?;
        (ssh, preexisted)
    } else {
        // No pre-connected client — connect now.
        let ssh_config = SshConfig::from_server(&server.host, server.port);
        let mut ssh = SshClient::new(ssh_config);
        let preexisted = if let Some(task_tree) = &task_tree {
            run_task_tree_deploy_step(
                task_tree,
                server_name,
                "connecting",
                connect_and_prepare_remote_release_dir(
                    &mut ssh,
                    &release_dir,
                    &config.shared_dir(),
                ),
            )
            .await?
        } else {
            run_deploy_step(
                "Preflight",
                "Preflight",
                use_spinner,
                connect_and_prepare_remote_release_dir(
                    &mut ssh,
                    &release_dir,
                    &config.shared_dir(),
                ),
            )
            .await?
        };
        (ssh, preexisted)
    };
    let archive_size_bytes = std::fs::metadata(archive_path)?.len();
    tracing::debug!("Archive size: {}", format_size(archive_size_bytes));
    let mut cleaned_partial_release = false;

    let result = async {
        // Upload artifact (skip if release dir already has it from a previous deploy).
        let remote_archive = remote_release_archive_path(&release_dir);
        if release_dir_preexisted {
            tracing::debug!("Release dir already exists, skipping upload");
            if let Some(task_tree) = &task_tree {
                task_tree.warn_deploy_step(server_name, "uploading", "cached");
            }
        } else {
            tracing::debug!("Uploading artifact ({})…", format_size(archive_size_bytes));
            let upload_timer = output::timed("Artifact upload");
            if let Some(task_tree) = &task_tree {
                let total_size = archive_size_bytes;
                let tt = task_tree.clone();
                let sn = server_name.to_string();
                let upload_started_at = Instant::now();
                run_task_tree_deploy_step_with_detail(
                    task_tree,
                    server_name,
                    "uploading",
                    None,
                    async {
                        ssh.upload_with_progress(
                            archive_path,
                            &remote_archive,
                            Some(Box::new(move |done, _total| {
                                let fraction = if total_size > 0 {
                                    done as f64 / total_size as f64
                                } else {
                                    0.0
                                };
                                tt.update_deploy_step_progress(
                                    &sn,
                                    "uploading",
                                    output::format_transfer_compact_detail(
                                        done,
                                        total_size,
                                        upload_started_at.elapsed(),
                                    ),
                                    fraction,
                                );
                            })),
                        )
                        .await
                        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })
                    },
                )
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                    format_deploy_step_failure("Uploading", &e.to_string()).into()
                })?;
            } else {
                let upload_result: Result<(), Box<dyn std::error::Error + Send + Sync>> =
                    if use_spinner {
                        let tp = std::sync::Arc::new(output::TransferProgress::new(
                            "Uploading",
                            "Uploaded",
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
                        Ok(())
                    } else {
                        ssh.upload(archive_path, &remote_archive).await.map_err(
                            |e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) },
                        )
                    };
                upload_result.map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                    format_deploy_step_failure("Uploading", &e.to_string()).into()
                })?;
            }
            drop(upload_timer);
        }

        // Extract archive and symlink shared dirs.
        if !release_dir_preexisted {
            if let Some(task_tree) = &task_tree {
                run_task_tree_deploy_step(task_tree, server_name, "preparing", async {
                    tracing::debug!("Extracting and configuring release…");
                    let extract_cmd =
                        build_remote_extract_archive_command(&release_dir, &remote_archive);
                    let shared_link_cmd = format!(
                        "mkdir -p {}/logs && ln -sfn {}/logs {}/logs",
                        config.shared_dir(),
                        config.shared_dir(),
                        release_dir
                    );
                    let combined_cmd = format!("{} && {}", extract_cmd, shared_link_cmd);
                    ssh.exec_checked(&combined_cmd).await?;

                    // Download runtime and install production dependencies on the server.
                    let prepare_cmd = Command::PrepareRelease {
                        app: config.app_name.clone(),
                        path: release_dir.clone(),
                    };
                    let json = serde_json::to_string(&prepare_cmd)
                        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
                    let response = ssh
                        .tako_command(&json)
                        .await
                        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
                    if deploy_response_has_error(&response) {
                        return Err(extract_server_error_message(&response).into());
                    }

                    Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                })
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                    format_deploy_step_failure("Preparing", &e.to_string()).into()
                })?;
            } else {
                run_deploy_step("Preparing…", "Prepared", use_spinner, async {
                    tracing::debug!("Extracting and configuring release…");
                    let extract_cmd =
                        build_remote_extract_archive_command(&release_dir, &remote_archive);
                    let shared_link_cmd = format!(
                        "mkdir -p {}/logs && ln -sfn {}/logs {}/logs",
                        config.shared_dir(),
                        config.shared_dir(),
                        release_dir
                    );
                    let combined_cmd = format!("{} && {}", extract_cmd, shared_link_cmd);
                    ssh.exec_checked(&combined_cmd).await?;

                    // Download runtime and install production dependencies on the server.
                    let prepare_cmd = Command::PrepareRelease {
                        app: config.app_name.clone(),
                        path: release_dir.clone(),
                    };
                    let json = serde_json::to_string(&prepare_cmd)
                        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
                    let response = ssh
                        .tako_command(&json)
                        .await
                        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
                    if deploy_response_has_error(&response) {
                        return Err(extract_server_error_message(&response).into());
                    }

                    Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                })
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
                    format_deploy_step_failure("Preparing", &e.to_string()).into()
                })?;
            }
        } else if let Some(task_tree) = &task_tree {
            task_tree.warn_deploy_step(server_name, "preparing", "skipped");
        }
        tracing::debug!(
            "{}",
            format_deploy_main_message(
                &config.main,
                target_label,
                config.use_unified_target_process,
            )
        );

        // Resolve secrets before the starting step to keep it fast.
        let deploy_secrets = match query_remote_secrets_hash(&ssh, &config.app_name).await {
            Some(remote_hash) if remote_hash == config.secrets_hash => None,
            _ => Some(config.env_vars.clone()),
        };

        let start_result = if let Some(task_tree) = &task_tree {
            run_task_tree_deploy_step_with_detail_and_error_cleanup(
                task_tree,
                server_name,
                "starting",
                None,
                async {
                    let cmd = Command::Deploy {
                        app: config.app_name.clone(),
                        version: config.version.clone(),
                        path: release_dir.clone(),
                        routes: config.routes.clone(),
                        secrets: deploy_secrets,
                    };
                    let json = serde_json::to_string(&cmd)
                        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
                    let response = ssh
                        .tako_command(&json)
                        .await
                        .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

                    if deploy_response_has_error(&response) {
                        return Err(extract_server_error_message(&response).into());
                    }

                    Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
                },
                || {
                    cleaned_partial_release = true;
                    async {
                        if !release_dir_preexisted
                            && let Err(e) = cleanup_partial_release(&ssh, &release_dir).await
                        {
                            tracing::warn!(
                                "Failed to cleanup partial release directory {release_dir}: {e}"
                            );
                        }
                    }
                },
            )
            .await
        } else {
            run_deploy_step("Starting…", "Started", use_spinner, async {
                let cmd = Command::Deploy {
                    app: config.app_name.clone(),
                    version: config.version.clone(),
                    path: release_dir.clone(),
                    routes: config.routes.clone(),
                    secrets: deploy_secrets,
                };
                let json = serde_json::to_string(&cmd)
                    .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;
                let response = ssh
                    .tako_command(&json)
                    .await
                    .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

                if deploy_response_has_error(&response) {
                    return Err(extract_server_error_message(&response).into());
                }

                Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
            })
            .await
        };
        start_result.map_err(|e| -> Box<dyn std::error::Error + Send + Sync> {
            format_deploy_step_failure("Starting", &e.to_string()).into()
        })?;

        // Post-deploy housekeeping (not timed as part of "Starting").
        ssh.symlink(&release_dir, &config.current_link())
            .await
            .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { Box::new(e) })?;

        let releases_dir = format!("{}/releases", config.remote_base);
        let cleanup_cmd = format!(
            "find {} -mindepth 1 -maxdepth 1 -type d -mtime +30 -exec rm -rf {{}} \\;",
            releases_dir
        );
        if let Err(e) = ssh.exec(&cleanup_cmd).await {
            tracing::warn!("Failed to clean up old releases: {e}");
        }

        if let Some(task_tree) = &task_tree {
            task_tree.succeed_deploy_target(server_name, None);
        }

        Ok(())
    }
    .await;

    if result.is_err()
        && !release_dir_preexisted
        && !cleaned_partial_release
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
            "javascript/tanstack-start@abc1234"
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
            "javascript/tanstack-start"
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

        persist_server_env_mapping(
            &temp_dir.path().join("tako.toml"),
            "tako-server",
            "production",
        )
        .unwrap();

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
            &temp_dir.path().join("tako.toml"),
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
            &temp_dir.path().join("tako.toml"),
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
            main: "index.ts".to_string(),
            use_unified_target_process: false,
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
    fn should_use_unified_js_target_process_only_for_js_runtimes() {
        assert!(should_use_unified_js_target_process("bun"));
        assert!(should_use_unified_js_target_process("node"));
        assert!(should_use_unified_js_target_process("deno"));
        assert!(!should_use_unified_js_target_process("go"));
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
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("App") && lines[0].contains("bun"));
        assert!(lines[1].contains("Target") && lines[1].contains("testbed"));
        assert!(lines[2].contains("Host") && lines[2].contains("tako@localhost:2222"));
    }

    #[test]
    fn deploy_overview_lines_include_server_count_for_multi_target() {
        let lines = format_deploy_overview_lines("bun", "staging", 3, None);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("App") && lines[0].contains("bun"));
        assert!(lines[1].contains("Target") && lines[1].contains("3 servers"));
    }

    #[test]
    fn deploy_summary_lines_keep_urls_literal_and_contiguous() {
        let lines = format_deploy_summary_lines(
            "Release",
            "20260330",
            &[
                "app.tako.test".to_string(),
                "app.tako.test/bun".to_string(),
                "*.app.tako.test".to_string(),
            ],
        );

        assert_eq!(
            lines,
            vec![
                SummaryLine {
                    label: "Release".to_string(),
                    value: "20260330".to_string(),
                },
                SummaryLine {
                    label: "Routes".to_string(),
                    value: "https://app.tako.test".to_string(),
                },
                SummaryLine {
                    label: String::new(),
                    value: "https://app.tako.test/bun".to_string(),
                },
                SummaryLine {
                    label: String::new(),
                    value: "https://*.app.tako.test".to_string(),
                },
            ]
        );
    }

    fn sample_shared_build_group() -> ArtifactBuildGroup {
        ArtifactBuildGroup {
            build_target_label: "linux-aarch64-musl".to_string(),
            cache_target_label: UNIFIED_JS_CACHE_TARGET_LABEL.to_string(),
            target_labels: vec!["linux-aarch64-musl".to_string()],
            display_target_label: None,
        }
    }

    fn sample_multi_build_groups() -> Vec<ArtifactBuildGroup> {
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
    }

    #[test]
    fn deploy_task_tree_initial_lines_include_known_future_work() {
        let controller = DeployTaskTreeController::new(
            &["tako-demo".to_string()],
            &[sample_shared_build_group()],
        );
        let snapshot = controller.snapshot();
        let lines = ui::render_plain_lines(&build_deploy_tree(&snapshot));

        assert_eq!(
            lines,
            vec![
                "○ Building...".to_string(),
                String::new(),
                "○ Deploying to tako-demo...".to_string(),
                "  ○ Preflight...".to_string(),
                "  ○ Uploading...".to_string(),
                "  ○ Preparing...".to_string(),
                "  ○ Starting...".to_string(),
            ]
        );
    }

    #[test]
    fn deploy_task_tree_initial_lines_include_multi_target_builds_and_multi_server_children() {
        let controller = DeployTaskTreeController::new(
            &["prod-a".to_string(), "prod-b".to_string()],
            &sample_multi_build_groups(),
        );
        let snapshot = controller.snapshot();
        let lines = ui::render_plain_lines(&build_deploy_tree(&snapshot));

        // No top-level Preflight — it's a child of each deploy group
        assert!(!lines.iter().any(|line| line == "○ Preflight..."));
        assert!(lines.iter().any(|line| line == "○ Building..."));
        assert!(lines.iter().any(|line| line == "  ○ linux-aarch64-musl..."));
        assert!(lines.iter().any(|line| line == "  ○ linux-x86_64-glibc..."));
        assert!(lines.iter().any(|line| line == "○ Deploying to prod-a..."));
        assert!(lines.iter().any(|line| line == "○ Deploying to prod-b..."));
        // Each deploy group has Preflight + Uploading + Preparing + Starting
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.contains("○ Preflight..."))
                .count(),
            2
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.contains("○ Uploading..."))
                .count(),
            2
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.contains("○ Preparing..."))
                .count(),
            2
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.contains("○ Starting..."))
                .count(),
            2
        );
    }

    #[test]
    fn deploy_task_tree_marks_build_as_running_before_build_steps_start() {
        let controller = DeployTaskTreeController::new(
            &["tako-demo".to_string()],
            &[sample_shared_build_group()],
        );

        controller.mark_build_target_running("shared target");

        let snapshot = controller.snapshot();
        let lines = ui::render_plain_lines(&build_deploy_tree(&snapshot));

        assert!(lines.iter().any(|line| line.starts_with("✶ Building")));
        let build = snapshot
            .builds
            .iter()
            .find(|task| task.label == "shared target")
            .unwrap();
        assert!(matches!(build.state, TaskState::Running { .. }));
        assert!(
            build
                .children
                .iter()
                .all(|child| matches!(child.state, TaskState::Pending))
        );
    }

    #[test]
    fn deploy_task_tree_cache_hit_appends_completed_cached_artifact_step() {
        let controller =
            DeployTaskTreeController::new(&["prod-a".to_string()], &[sample_shared_build_group()]);

        controller.succeed_build_step("shared target", "probe-runtime", Some("bun 1.2.3".into()));
        controller.warn_build_step("shared target", "build-artifact", "skipped");
        controller.warn_build_step("shared target", "package-artifact", "skipped");
        controller.append_cached_artifact_step("shared target", Some("72 MB".to_string()));
        controller.succeed_build_target("shared target", Some("72 MB (cached)".to_string()));

        let snapshot = controller.snapshot();
        let cached_step = snapshot
            .builds
            .iter()
            .find_map(|task| task.find(&build_task_step_id("shared target", "use-cached-artifact")))
            .unwrap();
        assert!(matches!(cached_step.state, TaskState::Succeeded { .. }));
        let lines = ui::render_plain_lines(&build_deploy_tree(&snapshot));
        // The "Built" line should include the detail (e.g. "Cached 72 MB")
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with("✔ Built") && line.contains("72 MB (cached)")),
            "built line should show detail: {lines:?}"
        );
        let built_index = lines
            .iter()
            .position(|line| line.starts_with("✔ Built"))
            .unwrap();
        assert_eq!(lines.get(built_index + 1), Some(&String::new()));
        assert_eq!(
            lines.get(built_index + 2),
            Some(&"○ Deploying to prod-a...".to_string())
        );
        assert_eq!(lines.last(), Some(&"  ○ Starting...".to_string()));
    }

    #[test]
    fn deploy_task_tree_can_show_parallel_running_rows() {
        let controller = DeployTaskTreeController::new(
            &["prod-a".to_string(), "prod-b".to_string()],
            &[sample_shared_build_group()],
        );

        controller.mark_deploy_step_running("prod-a", "uploading");
        controller.mark_deploy_step_running("prod-b", "uploading");

        let snapshot = controller.snapshot();
        let lines = ui::render_plain_lines(&build_deploy_tree(&snapshot));
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with("✶ Deploying to prod-"))
                .count(),
            2
        );
        assert_eq!(
            lines
                .iter()
                .filter(|line| line.starts_with("  ✶ Uploading"))
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn deploy_task_tree_marks_preflight_running_before_complete() {
        let controller =
            DeployTaskTreeController::new(&["prod-a".to_string()], &[sample_shared_build_group()]);
        let worker_controller = controller.clone();
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();

        let handle = tokio::spawn(async move {
            run_task_tree_deploy_step(&worker_controller, "prod-a", "connecting", async move {
                rx.await.expect("test signal should arrive");
                Ok::<(), String>(())
            })
            .await
            .expect("preflight step should succeed");
        });

        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                let snapshot = controller.snapshot();
                let deploy_target = snapshot
                    .deploys
                    .iter()
                    .find(|task| task.id == deploy_target_task_id("prod-a"))
                    .expect("deploy target should exist");
                let preflight = deploy_target
                    .find(&deploy_task_step_id("prod-a", "connecting"))
                    .expect("preflight step should exist");
                if matches!(deploy_target.state, TaskState::Running { .. })
                    && matches!(preflight.state, TaskState::Running { .. })
                {
                    assert_eq!(preflight.label, "Preflight");
                    return;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("preflight step should enter running state");

        tx.send(()).expect("worker should still be waiting");
        handle.await.expect("worker should finish cleanly");

        let snapshot = controller.snapshot();
        let deploy_target = snapshot
            .deploys
            .iter()
            .find(|task| task.id == deploy_target_task_id("prod-a"))
            .expect("deploy target should exist");
        let preflight = deploy_target
            .find(&deploy_task_step_id("prod-a", "connecting"))
            .expect("preflight step should exist");
        assert_eq!(preflight.label, "Preflight");
        assert!(matches!(preflight.state, TaskState::Succeeded { .. }));
    }

    #[test]
    fn deploy_task_tree_shows_preflight_errors_under_deploy_group() {
        let controller =
            DeployTaskTreeController::new(&["prod-a".to_string()], &[sample_shared_build_group()]);

        controller.fail_preflight_check("prod-a", "SSH protocol error");

        let snapshot = controller.snapshot();
        let lines = ui::render_plain_lines(&build_deploy_tree(&snapshot));

        // Preflight failure is under the deploy group
        assert!(lines.iter().any(|line| line == "✘ Deploy to prod-a failed"));
        assert!(lines.iter().any(|line| line == "  ✘ Preflight failed"));
        assert!(!lines.iter().any(|line| line == "  SSH protocol error"));
        assert!(lines.iter().any(|line| line == "    SSH protocol error"));
    }

    #[test]
    fn deploy_task_tree_preflight_failure_aborts_remaining_deploy_children() {
        let controller =
            DeployTaskTreeController::new(&["prod-a".to_string()], &[sample_shared_build_group()]);

        controller.fail_preflight_check("prod-a", "SSH protocol error");

        let snapshot = controller.snapshot();
        let lines = ui::render_plain_lines(&build_deploy_tree(&snapshot));

        assert!(lines.iter().any(|line| line == "  ✘ Preflight failed"));
        assert!(
            lines
                .iter()
                .any(|line| line.contains("⏭ Uploading") && line.contains("skipped"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("⏭ Preparing") && line.contains("skipped"))
        );
        assert!(
            lines
                .iter()
                .any(|line| line.contains("⏭ Starting") && line.contains("skipped"))
        );
    }

    #[tokio::test]
    async fn deploy_task_tree_step_failure_attaches_detail_to_child_row() {
        let controller =
            DeployTaskTreeController::new(&["prod-a".to_string()], &[sample_shared_build_group()]);

        let err = run_task_tree_deploy_step(&controller, "prod-a", "starting", async {
            Err::<(), String>("Warm instance startup failed".to_string())
        })
        .await
        .unwrap_err();
        assert_eq!(err.to_string(), "Warm instance startup failed");

        let lines = ui::render_plain_lines(&build_deploy_tree(&controller.snapshot()));
        assert!(lines.iter().any(|line| line == "✘ Deploy to prod-a failed"));
        assert!(
            !lines
                .iter()
                .any(|line| line == "  Warm instance startup failed")
        );
        assert!(lines.iter().any(|line| line == "  ✘ Start failed"));
        assert!(
            lines
                .iter()
                .any(|line| line == "    Warm instance startup failed")
        );
    }

    #[tokio::test]
    async fn deploy_task_tree_defers_failed_start_state_until_cleanup_finishes() {
        let controller =
            DeployTaskTreeController::new(&["prod-a".to_string()], &[sample_shared_build_group()]);
        let (cleanup_started_tx, cleanup_started_rx) = tokio::sync::oneshot::channel();
        let (cleanup_finish_tx, cleanup_finish_rx) = tokio::sync::oneshot::channel::<()>();

        let task = tokio::spawn({
            let controller = controller.clone();
            async move {
                run_task_tree_deploy_step_with_detail_and_error_cleanup(
                    &controller,
                    "prod-a",
                    "starting",
                    None,
                    async { Err::<(), String>("Warm instance startup failed".to_string()) },
                    move || async move {
                        let _ = cleanup_started_tx.send(());
                        let _ = cleanup_finish_rx.await;
                    },
                )
                .await
                .unwrap_err()
            }
        });

        cleanup_started_rx.await.unwrap();

        let lines = ui::render_plain_lines(&build_deploy_tree(&controller.snapshot()));
        assert!(
            lines
                .iter()
                .any(|line| line.ends_with("Deploying to prod-a…"))
        );
        assert!(lines.iter().any(|line| line.contains("Starting…")));
        assert!(!lines.iter().any(|line| line == "  ✘ Start failed"));
        assert!(
            !lines
                .iter()
                .any(|line| line == "    Warm instance startup failed")
        );

        let _ = cleanup_finish_tx.send(());
        let err = task.await.unwrap();
        assert_eq!(err.to_string(), "Warm instance startup failed");

        let lines = ui::render_plain_lines(&build_deploy_tree(&controller.snapshot()));
        assert!(lines.iter().any(|line| line == "✘ Deploy to prod-a failed"));
        assert!(lines.iter().any(|line| line == "  ✘ Start failed"));
        assert!(
            lines
                .iter()
                .any(|line| line == "    Warm instance startup failed")
        );
    }

    #[test]
    fn deploy_task_tree_success_summary_appends_release_and_routes() {
        let controller =
            DeployTaskTreeController::new(&["prod-a".to_string()], &[sample_shared_build_group()]);
        controller.set_success_summary(
            "20260330",
            &["app.tako.test".to_string(), "*.app.tako.test".to_string()],
        );

        let lines = ui::render_plain_lines(&build_deploy_tree(&controller.snapshot()));
        assert!(
            lines.iter().any(|line| line == "  Release 20260330"),
            "expected '  Release 20260330' in {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "  Routes  https://app.tako.test"),
            "expected '  Routes  https://app.tako.test' in {lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|line| line == "          https://*.app.tako.test"),
            "expected continuation route in {lines:?}"
        );
    }

    #[test]
    fn deploy_task_tree_can_append_error_summary_line() {
        let controller =
            DeployTaskTreeController::new(&["prod-a".to_string()], &[sample_shared_build_group()]);
        controller.fail_preflight_check("prod-a", "SSH protocol error");
        controller.set_error_summary("Deployed to 0/1 servers".to_string());

        let lines = ui::render_plain_lines(&build_deploy_tree(&controller.snapshot()));
        assert_eq!(
            lines.get(lines.len().saturating_sub(2)),
            Some(&String::new())
        );
        assert_eq!(lines.last(), Some(&"Deployed to 0/1 servers".to_string()));
    }

    #[test]
    fn deploy_task_tree_build_failure_aborts_deploy_work() {
        let controller =
            DeployTaskTreeController::new(&["prod-a".to_string()], &[sample_shared_build_group()]);

        controller.mark_deploy_step_running("prod-a", "connecting");
        controller.fail_build_step("shared target", "build-artifact", "Local build failed");
        controller.fail_build_target("shared target", "Local build failed");
        controller.warn_pending_build_children("shared target", "skipped");
        controller.abort_incomplete("Aborted");

        let snapshot = controller.snapshot();
        let lines = ui::render_plain_lines(&build_deploy_tree(&snapshot));

        assert!(lines.iter().any(|line| line == "✘ Building"));
        assert!(lines.iter().any(|line| line == "  Local build failed"));
        // Preflight was running, gets cancelled by abort
        assert!(lines.iter().any(|line| line == "  ⏭ Preflight…"));
        assert!(lines.iter().any(|line| line == "  ⏭ Uploading…"));
        assert!(lines.iter().any(|line| line == "  ⏭ Preparing…"));
        assert!(lines.iter().any(|line| line == "  ⏭ Starting…"));
    }

    #[test]
    fn deploy_task_tree_omits_startup_summary_lines() {
        let controller =
            DeployTaskTreeController::new(&["prod-a".to_string()], &[sample_shared_build_group()]);
        let snapshot = controller.snapshot();
        let lines = ui::render_plain_lines(&build_deploy_tree(&snapshot));

        assert!(!lines.iter().any(|line| line.contains("https://")));
        assert!(!lines.iter().any(|line| line.contains("App")));
        assert!(!lines.iter().any(|line| line.contains("Env")));
    }

    #[test]
    fn deploy_summary_lines_support_non_url_primary_field() {
        let lines = format_deploy_summary_lines("App", "bun", &["app.tako.test".to_string()]);

        assert_eq!(
            lines,
            vec![
                SummaryLine {
                    label: "App".to_string(),
                    value: "bun".to_string(),
                },
                SummaryLine {
                    label: "Routes".to_string(),
                    value: "https://app.tako.test".to_string(),
                },
            ]
        );
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
            "Built for linux-aarch64-musl"
        );
        assert_eq!(
            format_prepare_artifact_message(Some("linux-aarch64-musl")),
            "Preparing artifact for linux-aarch64-musl"
        );
    }

    #[test]
    fn artifact_progress_helpers_render_shared_messages_without_target_label() {
        assert_eq!(format_build_artifact_message(None), "Building artifact");
        assert_eq!(format_build_completed_message(None), "Built");
        assert_eq!(format_prepare_artifact_message(None), "Preparing artifact");
    }

    #[test]
    fn should_use_per_server_spinners_only_for_single_interactive_target() {
        assert!(should_use_per_server_spinners(1, true));
        assert!(!should_use_per_server_spinners(2, true));
        assert!(!should_use_per_server_spinners(1, false));
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
        let project = Path::new("/repo/examples/javascript/bun");
        let artifact = Path::new("/repo/examples/javascript/bun/.tako/artifacts/a.tar.zst");
        assert_eq!(
            format_path_relative_to(project, artifact),
            ".tako/artifacts/a.tar.zst"
        );
    }

    #[test]
    fn format_path_relative_to_falls_back_to_absolute_when_outside_project() {
        let project = Path::new("/repo/examples/javascript/bun");
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
            Some("bun".to_string()),
            Some("feat: ship it".to_string()),
            Some(false),
            HashMap::new(),
            HashMap::new(),
            None,
            String::new(),
            String::new(),
        );
        assert_eq!(manifest.idle_timeout, 300);
        assert_eq!(
            manifest.env_vars.get("TAKO_BUILD"),
            Some(&"v123".to_string())
        );
        assert_eq!(manifest.package_manager, Some("bun".to_string()));
        assert_eq!(
            manifest.env_vars.get("TAKO_ENV"),
            Some(&"production".to_string())
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
    fn extract_server_error_message_strips_leading_deploy_failed_prefix() {
        let response =
            r#"{"status":"error","message":"Deploy failed: Warm instance startup failed"}"#;
        assert_eq!(
            extract_server_error_message(response),
            "Warm instance startup failed"
        );
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

        let partial = format_partial_failure_error(2);
        assert_eq!(partial, "2 server(s) failed");
    }

    #[test]
    fn check_wildcard_dns_support_passes_without_wildcards() {
        let routes = vec!["api.example.com".to_string()];
        let checks = vec![ServerCheck {
            name: "prod-1".to_string(),
            mode: tako_core::UpgradeMode::Normal,
            dns_provider: None,
        }];
        assert!(check_wildcard_dns_support(&routes, &checks).is_ok());
    }

    #[test]
    fn check_wildcard_dns_support_passes_when_all_have_dns() {
        let routes = vec!["*.example.com".to_string()];
        let checks = vec![ServerCheck {
            name: "prod-1".to_string(),
            mode: tako_core::UpgradeMode::Normal,
            dns_provider: Some("cloudflare".to_string()),
        }];
        assert!(check_wildcard_dns_support(&routes, &checks).is_ok());
    }

    #[test]
    fn check_wildcard_dns_support_fails_when_server_lacks_dns() {
        let routes = vec!["*.example.com".to_string()];
        let checks = vec![
            ServerCheck {
                name: "prod-1".to_string(),
                mode: tako_core::UpgradeMode::Normal,
                dns_provider: Some("cloudflare".to_string()),
            },
            ServerCheck {
                name: "prod-2".to_string(),
                mode: tako_core::UpgradeMode::Normal,
                dns_provider: None,
            },
        ];
        let err = check_wildcard_dns_support(&routes, &checks).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("prod-2"), "should name the server: {msg}");
        assert!(
            msg.contains("setup-wildcard"),
            "should suggest the command: {msg}"
        );
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
    fn deploy_min_free_disk_is_256mb() {
        assert_eq!(DEPLOY_MIN_FREE_DISK_BYTES, 256 * 1024 * 1024);
    }

    #[test]
    fn cleanup_partial_release_command_uses_safe_single_quoted_path() {
        let cmd = cleanup_partial_release_command("/opt/tako/apps/a'b/releases/v1");
        assert!(cmd.contains("rm -rf"));
        assert!(cmd.contains("'\\''"));
        assert!(cmd.contains("/opt/tako/apps/"));
    }

    #[test]
    fn source_bundle_root_falls_back_to_runtime_project_root_without_git() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().join("app");
        std::fs::create_dir_all(&project_dir).unwrap();
        // No lockfile anywhere → falls back to project_dir itself
        assert_eq!(source_bundle_root(&project_dir, "bun"), project_dir);
    }

    #[test]
    fn source_bundle_root_walks_up_to_lockfile_without_git() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("monorepo");
        let project_dir = root.join("apps/web");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(root.join("bun.lock"), "").unwrap();
        // No git, but lockfile is at the monorepo root → returns lockfile root
        assert_eq!(source_bundle_root(&project_dir, "bun"), root);
    }

    #[test]
    fn normalize_asset_root_rejects_invalid_paths() {
        assert!(normalize_asset_root(" ").is_err());
        assert!(normalize_asset_root("/tmp/assets").is_err());
        assert!(normalize_asset_root("../assets").is_err());
    }

    #[test]
    fn should_run_bun_lockfile_preflight_runs_for_bun_runtime() {
        assert!(should_run_bun_lockfile_preflight(BuildAdapter::Bun));
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
            None,
            Some(true),
            app_env_vars,
            runtime_env_vars,
            Some(&secrets),
            String::new(),
            String::new(),
        );

        assert_eq!(manifest.app_name, "my-app");
        assert_eq!(manifest.environment, "staging");
        assert_eq!(manifest.version, "v1");
        assert_eq!(manifest.runtime, "bun");
        assert_eq!(manifest.main, "server/index.mjs");
        assert_eq!(manifest.idle_timeout, 600);
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
        // NODE_ENV keeps the user-provided value; runtime defaults only apply
        // for explicitly defined environments (production, development).
        assert_eq!(
            manifest.env_vars.get("NODE_ENV"),
            Some(&"production".to_string())
        );
        assert_eq!(
            manifest.env_vars.get("BUN_ENV"),
            Some(&"production".to_string())
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
