use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::config::ServerTarget;
use crate::output;
use crate::ui::{
    SUMMARY_INDENT, TaskItemState, TaskState, TaskTreeSession, TreeNode, TreeTextTone,
};

use super::format::{SummaryLine, format_build_plan_target_label, format_deploy_summary_lines};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ArtifactBuildGroup {
    pub(super) build_target_label: String,
    pub(super) cache_target_label: String,
    pub(super) target_labels: Vec<String>,
    pub(super) display_target_label: Option<String>,
}

pub(super) const UNIFIED_JS_CACHE_TARGET_LABEL: &str = "shared-local-js";

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct LocalArtifactCacheCleanupSummary {
    pub(super) removed_target_artifacts: usize,
    pub(super) removed_target_metadata: usize,
}

impl LocalArtifactCacheCleanupSummary {
    pub(super) fn total_removed(self) -> usize {
        self.removed_target_artifacts + self.removed_target_metadata
    }
}

#[derive(Debug, Clone)]
pub(super) struct DeployTaskTreeState {
    pub(super) builds: Vec<TaskItemState>,
    pub(super) deploys: Vec<TaskItemState>,
    pub(super) success_lines: Vec<SummaryLine>,
    pub(super) summary_line: Option<(String, TreeTextTone)>,
}

#[derive(Clone)]
pub(super) struct DeployTaskTreeController {
    state: Arc<Mutex<DeployTaskTreeState>>,
    session: Option<TaskTreeSession>,
}

#[derive(Clone, Copy)]
pub(super) enum DeployCompletionKind {
    Succeeded,
    Cancelled,
    Failed,
}

impl DeployTaskTreeController {
    pub(super) fn new(server_names: &[String], build_groups: &[ArtifactBuildGroup]) -> Self {
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

    pub(super) fn fail_preflight_check(&self, server_name: &str, detail: impl Into<String>) {
        let msg = detail.into();
        self.fail_deploy_step(server_name, "connecting", msg.clone());
        self.rename_deploy_step(server_name, "connecting", "Preflight failed");
        self.fail_deploy_target_without_detail(server_name);
        self.warn_pending_deploy_children(server_name, "skipped");
    }

    pub(super) fn mark_build_step_running(&self, target_label: &str, step: &str) {
        self.mark_running_by_id(&build_target_task_id(target_label));
        self.mark_running_by_id(&build_task_step_id(target_label, step));
    }

    pub(super) fn succeed_build_step(
        &self,
        target_label: &str,
        step: &str,
        detail: Option<String>,
    ) {
        self.complete_by_id(
            &build_task_step_id(target_label, step),
            detail,
            DeployCompletionKind::Succeeded,
        );
    }

    pub(super) fn warn_build_step(
        &self,
        target_label: &str,
        step: &str,
        detail: impl Into<String>,
    ) {
        self.complete_by_id(
            &build_task_step_id(target_label, step),
            Some(detail.into()),
            DeployCompletionKind::Cancelled,
        );
    }

    pub(super) fn fail_build_step(
        &self,
        target_label: &str,
        step: &str,
        detail: impl Into<String>,
    ) {
        self.complete_by_id(
            &build_task_step_id(target_label, step),
            Some(detail.into()),
            DeployCompletionKind::Failed,
        );
    }

    pub(super) fn append_cached_artifact_step(&self, target_label: &str, detail: Option<String>) {
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

    pub(super) fn succeed_build_target(&self, target_label: &str, detail: Option<String>) {
        self.complete_by_id(
            &build_target_task_id(target_label),
            detail,
            DeployCompletionKind::Succeeded,
        );
    }

    pub(super) fn mark_build_target_running(&self, target_label: &str) {
        self.mark_running_by_id(&build_target_task_id(target_label));
    }

    pub(super) fn fail_build_target(&self, target_label: &str, detail: impl Into<String>) {
        self.complete_by_id(
            &build_target_task_id(target_label),
            Some(detail.into()),
            DeployCompletionKind::Failed,
        );
    }

    pub(super) fn warn_pending_build_children(&self, target_label: &str, reason: &str) {
        self.warn_pending_children(&build_target_task_id(target_label), reason);
    }

    pub(super) fn mark_deploy_step_running(&self, server_name: &str, step: &str) {
        self.mark_running_by_id(&deploy_target_task_id(server_name));
        self.mark_running_by_id(&deploy_task_step_id(server_name, step));
    }

    pub(super) fn update_deploy_step_progress(
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

    pub(super) fn succeed_deploy_step(
        &self,
        server_name: &str,
        step: &str,
        detail: Option<String>,
    ) {
        self.complete_by_id(
            &deploy_task_step_id(server_name, step),
            detail,
            DeployCompletionKind::Succeeded,
        );
    }

    pub(super) fn warn_deploy_step(
        &self,
        server_name: &str,
        step: &str,
        detail: impl Into<String>,
    ) {
        self.complete_by_id(
            &deploy_task_step_id(server_name, step),
            Some(detail.into()),
            DeployCompletionKind::Cancelled,
        );
    }

    pub(super) fn fail_deploy_step(
        &self,
        server_name: &str,
        step: &str,
        detail: impl Into<String>,
    ) {
        self.complete_by_id(
            &deploy_task_step_id(server_name, step),
            Some(detail.into()),
            DeployCompletionKind::Failed,
        );
    }

    pub(super) fn rename_deploy_step(&self, server_name: &str, step: &str, new_label: &str) {
        self.update_by_id(&deploy_task_step_id(server_name, step), |task| {
            task.label = new_label.to_string();
        });
    }

    pub(super) fn succeed_deploy_target(&self, server_name: &str, detail: Option<String>) {
        self.complete_by_id(
            &deploy_target_task_id(server_name),
            detail,
            DeployCompletionKind::Succeeded,
        );
    }

    pub(super) fn fail_deploy_target_without_detail(&self, server_name: &str) {
        self.complete_by_id(
            &deploy_target_task_id(server_name),
            None,
            DeployCompletionKind::Failed,
        );
    }

    pub(super) fn warn_pending_deploy_children(&self, server_name: &str, reason: &str) {
        self.warn_pending_children(&deploy_target_task_id(server_name), reason);
    }

    pub(super) fn abort_incomplete(&self, reason: &str) {
        let mut state = self.state.lock().unwrap();
        abort_incomplete_tasks(&mut state.builds, reason);
        abort_incomplete_tasks(&mut state.deploys, reason);
        self.refresh_locked(&state);
    }

    pub(super) fn set_success_summary(&self, version: &str, routes: &[String]) {
        let mut state = self.state.lock().unwrap();
        state.success_lines = format_deploy_summary_lines("Release", version, routes);
        self.refresh_locked(&state);
    }

    pub(super) fn set_error_summary(&self, summary: String) {
        let mut state = self.state.lock().unwrap();
        state.summary_line = Some((summary, TreeTextTone::Error));
        self.refresh_locked(&state);
    }

    pub(super) fn finalize(&self) {
        if let Some(session) = &self.session {
            session.finalize();
        }
    }

    #[cfg(test)]
    pub(super) fn snapshot(&self) -> DeployTaskTreeState {
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

pub(super) fn should_use_deploy_task_tree() -> bool {
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

pub(super) fn build_target_task_id(target_label: &str) -> String {
    format!("build:{target_label}")
}

pub(super) fn build_task_step_id(target_label: &str, step: &str) -> String {
    format!("build:{target_label}:{step}")
}

pub(super) fn deploy_target_task_id(server_name: &str) -> String {
    format!("deploy:{server_name}")
}

pub(super) fn deploy_task_step_id(server_name: &str, step: &str) -> String {
    format!("deploy:{server_name}:{step}")
}

/// Build the render tree from deploy state. This replaces the old UiNode-based
/// `build_deploy_task_tree_root`. Controllers call this via `refresh_locked()`.
pub(super) fn build_deploy_tree(state: &DeployTaskTreeState) -> Vec<TreeNode> {
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

pub(super) fn build_artifact_target_groups(
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

#[cfg(test)]
mod tests {
    use super::super::remote::run_task_tree_deploy_step;
    use super::super::remote::run_task_tree_deploy_step_with_detail_and_error_cleanup;
    use super::*;
    use crate::config::ServerTarget;
    use crate::ui;
    use std::time::Duration;

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
}
