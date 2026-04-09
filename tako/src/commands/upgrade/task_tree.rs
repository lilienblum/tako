use std::sync::{Arc, Mutex};

use crate::config::UpgradeChannel;
use crate::output;
use crate::ui::{TaskItemState, TaskState, TaskTreeSession, TreeNode};

use super::CliUpgradeMethod;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum UpgradeTaskId {
    CheckForUpdates,
    DownloadArchive,
    VerifySha256,
    ExtractArchive,
    InstallBinaries,
    UpgradeViaHomebrew,
}

#[derive(Debug, Clone)]
pub(super) struct UpgradeTaskTreeState {
    pub(super) tasks: Vec<TaskItemState>,
}

#[derive(Clone)]
pub(super) struct UpgradeTaskTreeController {
    state: Arc<Mutex<UpgradeTaskTreeState>>,
    session: Option<TaskTreeSession>,
}

impl UpgradeTaskId {
    pub(super) fn key(self) -> &'static str {
        match self {
            Self::CheckForUpdates => "check-for-updates",
            Self::DownloadArchive => "download-archive",
            Self::VerifySha256 => "verify-sha256",
            Self::ExtractArchive => "extract-archive",
            Self::InstallBinaries => "install-binaries",
            Self::UpgradeViaHomebrew => "upgrade-via-homebrew",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::CheckForUpdates => "Check for updates",
            Self::DownloadArchive => "Download archive",
            Self::VerifySha256 => "Verify SHA256",
            Self::ExtractArchive => "Extract archive",
            Self::InstallBinaries => "Install binaries",
            Self::UpgradeViaHomebrew => "Upgrade via Homebrew",
        }
    }
}

impl UpgradeTaskTreeController {
    pub(super) fn new(channel: UpgradeChannel, method: CliUpgradeMethod) -> Self {
        let state = UpgradeTaskTreeState {
            tasks: build_upgrade_tasks(channel, method),
        };
        let tree = build_upgrade_tree(&state);
        let session = should_use_upgrade_task_tree().then(|| TaskTreeSession::new(tree));
        Self {
            state: Arc::new(Mutex::new(state)),
            session,
        }
    }

    pub(super) fn mark_running(&self, id: UpgradeTaskId) {
        self.update(id, |task| {
            task.state = TaskState::Running {
                started_at: std::time::Instant::now(),
            };
            task.detail = None;
        });
    }

    pub(super) fn succeed(&self, id: UpgradeTaskId, detail: Option<String>) {
        self.complete(id, detail, CompletionKind::Succeeded);
    }

    pub(super) fn warn(&self, id: UpgradeTaskId, detail: Option<String>) {
        self.complete(id, detail, CompletionKind::Cancelled);
    }

    pub(super) fn fail(&self, id: UpgradeTaskId, detail: Option<String>) {
        self.complete(id, detail, CompletionKind::Failed);
    }

    pub(super) fn skip_pending(&self, reason: &str) {
        let mut guard = self.state.lock().unwrap();
        for task in &mut guard.tasks {
            if matches!(task.state, TaskState::Pending) {
                task.state = TaskState::Cancelled { elapsed: None };
                task.detail = Some(reason.to_string());
            }
        }
        self.refresh_locked(&guard);
    }

    #[cfg(test)]
    pub(super) fn snapshot(&self) -> UpgradeTaskTreeState {
        self.state.lock().unwrap().clone()
    }

    fn update<F>(&self, id: UpgradeTaskId, update: F)
    where
        F: FnOnce(&mut TaskItemState),
    {
        let mut guard = self.state.lock().unwrap();
        let task = guard
            .tasks
            .iter_mut()
            .find_map(|task| task.find_mut(id.key()))
            .unwrap_or_else(|| panic!("missing upgrade task {}", id.key()));
        update(task);
        self.refresh_locked(&guard);
    }

    fn complete(&self, id: UpgradeTaskId, detail: Option<String>, kind: CompletionKind) {
        self.update(id, |task| {
            let elapsed = match task.state {
                TaskState::Running { started_at } => Some(started_at.elapsed()),
                _ => None,
            };
            task.state = match kind {
                CompletionKind::Succeeded => TaskState::Succeeded { elapsed },
                CompletionKind::Cancelled => TaskState::Cancelled { elapsed },
                CompletionKind::Failed => TaskState::Failed { elapsed },
            };
            task.detail = detail;
        });
    }

    fn refresh_locked(&self, state: &UpgradeTaskTreeState) {
        if let Some(session) = &self.session {
            session.set_tree(build_upgrade_tree(state));
        }
    }
}

#[derive(Clone, Copy)]
enum CompletionKind {
    Succeeded,
    Cancelled,
    Failed,
}

fn build_upgrade_tasks(_channel: UpgradeChannel, method: CliUpgradeMethod) -> Vec<TaskItemState> {
    match method {
        CliUpgradeMethod::Installer => {
            vec![
                TaskItemState::pending(
                    UpgradeTaskId::CheckForUpdates.key(),
                    UpgradeTaskId::CheckForUpdates.label(),
                ),
                TaskItemState::pending(
                    UpgradeTaskId::DownloadArchive.key(),
                    UpgradeTaskId::DownloadArchive.label(),
                ),
                TaskItemState::pending(
                    UpgradeTaskId::VerifySha256.key(),
                    UpgradeTaskId::VerifySha256.label(),
                ),
                TaskItemState::pending(
                    UpgradeTaskId::ExtractArchive.key(),
                    UpgradeTaskId::ExtractArchive.label(),
                ),
                TaskItemState::pending(
                    UpgradeTaskId::InstallBinaries.key(),
                    UpgradeTaskId::InstallBinaries.label(),
                ),
            ]
        }
        CliUpgradeMethod::Homebrew => vec![TaskItemState::pending(
            UpgradeTaskId::UpgradeViaHomebrew.key(),
            UpgradeTaskId::UpgradeViaHomebrew.label(),
        )],
    }
}

fn build_upgrade_tree(state: &UpgradeTaskTreeState) -> Vec<TreeNode> {
    state
        .tasks
        .iter()
        .map(|task| TreeNode::Task(task.clone()))
        .collect()
}

pub(super) fn should_use_upgrade_task_tree() -> bool {
    should_use_upgrade_task_tree_for_mode(output::is_pretty(), output::is_interactive())
}

pub(super) fn should_use_upgrade_task_tree_for_mode(pretty: bool, interactive: bool) -> bool {
    pretty && interactive
}
