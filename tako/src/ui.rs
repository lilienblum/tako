use std::io;
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use ratatui::backend::CrosstermBackend;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget, Wrap};
use ratatui::{Terminal, TerminalOptions, Viewport};

use crate::output;

const TASK_INDENT: &str = "  ";
const TASK_SPINNER_TICKS: &[&str] = &["✶", "✸", "✹", "✺", "✹", "✷"];
const LIVE_RENDER_INTERVAL: Duration = Duration::from_millis(80);

/// Row below the active inline viewport, updated on each draw.
static VIEWPORT_BOTTOM_ROW: AtomicU16 = AtomicU16::new(0);
static ACTIVE_SESSION: Mutex<Option<Weak<SessionShared>>> = Mutex::new(None);

/// Minimal cleanup for Ctrl-C: move cursor below the viewport.
pub fn cleanup_on_interrupt() {
    let row = VIEWPORT_BOTTOM_ROW.load(Ordering::Relaxed);
    if row > 0 {
        let _ = crossterm::execute!(io::stderr(), crossterm::cursor::MoveTo(0, row));
    }
}

// Brand colors (matching output.rs)
const COLOR_ACCENT: Color = Color::Rgb(125, 196, 228);
const COLOR_SUCCESS: Color = Color::Rgb(155, 217, 179);
const COLOR_ERROR: Color = Color::Rgb(232, 163, 160);

// ── State types (unchanged) ─────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskState {
    Pending,
    Running { started_at: Instant },
    Succeeded { elapsed: Option<Duration> },
    Failed { elapsed: Option<Duration> },
    Cancelled { elapsed: Option<Duration> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct TaskItemState {
    pub id: String,
    pub label: String,
    pub state: TaskState,
    pub detail: Option<String>,
    /// Progress fraction (0.0–1.0) rendered as a native ratatui block bar.
    pub progress: Option<f64>,
    pub children: Vec<TaskItemState>,
}

impl TaskItemState {
    pub fn pending(id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            state: TaskState::Pending,
            detail: None,
            progress: None,
            children: Vec::new(),
        }
    }

    pub fn with_children(mut self, children: Vec<TaskItemState>) -> Self {
        self.children = children;
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn append_child(&mut self, child: TaskItemState) {
        self.children.push(child);
    }

    pub fn find(&self, id: &str) -> Option<&TaskItemState> {
        if self.id == id {
            return Some(self);
        }
        self.children.iter().find_map(|child| child.find(id))
    }

    pub fn find_mut(&mut self, id: &str) -> Option<&mut TaskItemState> {
        if self.id == id {
            return Some(self);
        }
        self.children
            .iter_mut()
            .find_map(|child| child.find_mut(id))
    }
}

// ── Render tree ─────────────────────────────────────────────────────────
//
// A lightweight tree that controllers build for rendering. Each node is either
// a task (leaf or group with children) or a spacer/text line. This replaces
// the old `UiNode` enum — controllers build this directly from their state.

#[derive(Debug, Clone)]
pub enum TreeTextTone {
    Error,
}

#[derive(Debug, Clone)]
pub enum TreeNode {
    /// A task item (leaf or group with children).
    Task(TaskItemState),
    /// An accent task item rendered as a top-level reporter (e.g., "Built 3.4s").
    AccentTask(TaskItemState),
    /// A non-task text row.
    Text { text: String, tone: TreeTextTone },
    /// Blank spacer line.
    Spacer,
}

// ── Task tree session (ratatui-based) ───────────────────────────────────

type RatatuiTerminal = Terminal<CrosstermBackend<io::Stderr>>;

#[derive(Clone)]
pub struct TaskTreeSession {
    shared: Arc<SessionShared>,
}

struct SessionShared {
    enabled: bool,
    stop: AtomicBool,
    state: Mutex<SessionState>,
    tick_thread: Mutex<Option<thread::JoinHandle<()>>>,
    terminal: Mutex<Option<RatatuiTerminal>>,
}

struct SessionState {
    tree: Vec<TreeNode>,
    paused: bool,
    frame_index: usize,
}

impl TaskTreeSession {
    pub fn new(tree: Vec<TreeNode>) -> Self {
        let enabled = output::is_pretty() && output::is_interactive();

        let terminal = if enabled {
            create_inline_terminal(&tree).ok()
        } else {
            None
        };

        let shared = Arc::new(SessionShared {
            enabled,
            stop: AtomicBool::new(false),
            state: Mutex::new(SessionState {
                tree,
                paused: false,
                frame_index: 0,
            }),
            tick_thread: Mutex::new(None),
            terminal: Mutex::new(terminal),
        });

        let session = Self {
            shared: shared.clone(),
        };

        if enabled {
            *ACTIVE_SESSION.lock().unwrap() = Some(Arc::downgrade(&shared));
            session.draw_now();
            let thread_shared = shared.clone();
            let handle = thread::spawn(move || {
                while !thread_shared.stop.load(Ordering::Relaxed) {
                    thread::sleep(LIVE_RENDER_INTERVAL);
                    if thread_shared.stop.load(Ordering::Relaxed) {
                        break;
                    }

                    let should_draw = {
                        let mut state = thread_shared.state.lock().unwrap();
                        let has_running = state.tree.iter().any(tree_node_has_running);
                        if state.paused || !has_running {
                            false
                        } else {
                            state.frame_index = (state.frame_index + 1) % TASK_SPINNER_TICKS.len();
                            true
                        }
                    };

                    if should_draw {
                        draw_shared(&thread_shared);
                    }
                }
            });
            *shared.tick_thread.lock().unwrap() = Some(handle);
        }

        session
    }

    pub fn set_tree(&self, tree: Vec<TreeNode>) {
        {
            let mut state = self.shared.state.lock().unwrap();
            state.tree = tree;
        }
        if self.shared.enabled {
            self.draw_now();
        }
    }

    pub fn pause(&self) {
        if !self.shared.enabled {
            return;
        }
        let mut state = self.shared.state.lock().unwrap();
        if state.paused {
            return;
        }
        state.paused = true;
        drop(state);

        if let Ok(mut term_guard) = self.shared.terminal.lock()
            && let Some(term) = term_guard.as_mut()
        {
            let _ = term.clear();
        }
    }

    pub fn resume(&self) {
        if !self.shared.enabled {
            return;
        }
        {
            let mut state = self.shared.state.lock().unwrap();
            if !state.paused {
                return;
            }
            state.paused = false;
        }
        self.draw_now();
    }

    fn draw_now(&self) {
        if !self.shared.enabled {
            return;
        }
        draw_shared(&self.shared);
    }
}

impl Drop for TaskTreeSession {
    fn drop(&mut self) {
        if Arc::strong_count(&self.shared) != 1 {
            return;
        }
        finalize_shared_session(&self.shared);
    }
}

pub fn interrupt_with_message(message: &str) -> bool {
    let shared = ACTIVE_SESSION
        .lock()
        .unwrap()
        .as_ref()
        .and_then(Weak::upgrade);
    let Some(shared) = shared else {
        return false;
    };
    if !shared.enabled {
        return false;
    }

    {
        let mut state = shared.state.lock().unwrap();
        append_interrupt_message(&mut state.tree, message);
    }
    draw_shared(&shared);
    true
}

pub fn finalize_active_session() -> bool {
    let shared = ACTIVE_SESSION
        .lock()
        .unwrap()
        .as_ref()
        .and_then(Weak::upgrade);
    let Some(shared) = shared else {
        return false;
    };
    finalize_shared_session(&shared);
    true
}

fn create_inline_terminal(tree: &[TreeNode]) -> io::Result<RatatuiTerminal> {
    let width = crossterm::terminal::size()?.0;
    let height = rendered_height(&render_tree_to_lines(tree, 0), width);
    let height = height.max(4).min(20); // at least 4, at most 20
    let backend = CrosstermBackend::new(io::stderr());
    Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Inline(height),
        },
    )
}

fn draw_shared(shared: &SessionShared) {
    let lines = {
        let state = shared.state.lock().unwrap();
        if state.paused {
            return;
        }
        render_tree_to_lines(&state.tree, state.frame_index)
    };

    if let Ok(mut term_guard) = shared.terminal.lock()
        && let Some(term) = term_guard.as_mut()
    {
        // Resize viewport if content grew
        let width = term.size().map(|s| s.width).unwrap_or(80);
        let needed = rendered_height(&lines, width);
        let current = term.size().map(|s| s.height).unwrap_or(0);
        if needed > current {
            let _ = term.resize(ratatui::layout::Rect::new(
                0,
                0,
                term.size().map(|s| s.width).unwrap_or(80),
                needed.min(20),
            ));
        }
        let _ = term.draw(|frame| {
            let paragraph = Paragraph::new(lines).wrap(Wrap { trim: false });
            frame.render_widget(paragraph, frame.area());
        });
        let area = term.get_frame().area();
        VIEWPORT_BOTTOM_ROW.store(area.y + area.height, Ordering::Relaxed);
    }
}

fn rendered_height(lines: &[Line<'_>], width: u16) -> u16 {
    let width = width.max(1) as usize;
    lines.iter().fold(0u16, |total, line| {
        let line_width = line.width();
        let rows = if line_width == 0 {
            1
        } else {
            line_width.div_ceil(width) as u16
        };
        total.saturating_add(rows)
    })
}

fn finalize_shared_session(shared: &Arc<SessionShared>) {
    {
        let mut active = ACTIVE_SESSION.lock().unwrap();
        let is_this_session = active
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some_and(|a| Arc::ptr_eq(&a, shared));
        if is_this_session {
            *active = None;
        }
    }

    shared.stop.store(true, Ordering::Relaxed);
    if let Some(handle) = shared.tick_thread.lock().unwrap().take() {
        let _ = handle.join();
    }

    if !shared.enabled {
        return;
    }

    if let Ok(mut term_guard) = shared.terminal.lock()
        && let Some(term) = term_guard.as_mut()
    {
        let lines = {
            let state = shared.state.lock().unwrap();
            render_tree_to_lines(&state.tree, state.frame_index)
        };
        let width = term.size().map(|s| s.width).unwrap_or(80);
        let height = rendered_height(&lines, width);
        let _ = term.insert_before(height, |buf| {
            Paragraph::new(lines.clone())
                .wrap(Wrap { trim: false })
                .render(buf.area, buf);
        });
    }
}

fn append_interrupt_message(tree: &mut Vec<TreeNode>, message: &str) {
    let already_appended = matches!(
        tree.last(),
        Some(TreeNode::Text { text, tone: TreeTextTone::Error }) if text == message
    );
    if already_appended {
        return;
    }

    // Cancel any in-progress tasks so they render with the cancelled icon.
    for node in tree.iter_mut() {
        match node {
            TreeNode::Task(task) | TreeNode::AccentTask(task) => {
                cancel_running_task(task);
            }
            _ => {}
        }
    }

    if !tree.is_empty() && !matches!(tree.last(), Some(TreeNode::Spacer)) {
        tree.push(TreeNode::Spacer);
    }
    tree.push(TreeNode::Text {
        text: message.to_string(),
        tone: TreeTextTone::Error,
    });
}

fn cancel_running_task(task: &mut TaskItemState) {
    for child in &mut task.children {
        cancel_running_task(child);
    }
    if let TaskState::Running { started_at } = task.state {
        task.state = TaskState::Cancelled {
            elapsed: Some(started_at.elapsed()),
        };
    }
}

fn tree_node_has_running(node: &TreeNode) -> bool {
    match node {
        TreeNode::Task(task) | TreeNode::AccentTask(task) => {
            matches!(task.state, TaskState::Running { .. })
                || task.children.iter().any(task_item_has_running)
        }
        TreeNode::Text { .. } => false,
        TreeNode::Spacer => false,
    }
}

fn task_item_has_running(task: &TaskItemState) -> bool {
    matches!(task.state, TaskState::Running { .. })
        || task.children.iter().any(task_item_has_running)
}

// ── Rendering: TaskItemState → ratatui Line/Span ────────────────────────

fn render_tree_to_lines(tree: &[TreeNode], frame_index: usize) -> Vec<Line<'static>> {
    let now = Instant::now();
    let mut lines = Vec::new();
    for node in tree {
        match node {
            TreeNode::Task(task) => {
                render_task_item(&mut lines, task, "", false, now, frame_index);
            }
            TreeNode::AccentTask(task) => {
                render_task_item(&mut lines, task, "", true, now, frame_index);
            }
            TreeNode::Text { text, tone } => {
                let style = match tone {
                    TreeTextTone::Error => Style::new().fg(COLOR_ERROR),
                };
                lines.push(Line::from(vec![Span::styled(text.clone(), style)]));
            }
            TreeNode::Spacer => {
                lines.push(Line::raw(""));
            }
        }
    }
    lines
}

fn render_task_item(
    lines: &mut Vec<Line<'static>>,
    task: &TaskItemState,
    prefix: &str,
    accent: bool,
    now: Instant,
    frame_index: usize,
) {
    let is_group = !task.children.is_empty();

    // Build the main task line
    let icon = task_icon(&task.state, frame_index);
    let label = pending_task_label(&task.label, &task.state);
    let detail_suffix = format_detail_suffix(task, now);

    let (icon_style, label_style, detail_style) = task_line_styles(&task.state, is_group || accent);

    let mut spans = vec![
        Span::styled(format!("{prefix}{icon}"), icon_style),
        Span::styled(format!(" {label}"), label_style),
    ];
    if let Some(fraction) = task.progress {
        spans.push(Span::raw(" "));
        render_block_bar_spans(&mut spans, fraction);
    }
    if !detail_suffix.is_empty() {
        spans.push(Span::styled(format!(" {detail_suffix}"), detail_style));
    }
    lines.push(Line::from(spans));

    // Error detail line for failed leaf tasks and groups
    if matches!(task.state, TaskState::Failed { .. })
        && let Some(detail) = task
            .detail
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
    {
        lines.push(Line::from(vec![Span::styled(
            format!("{prefix}{TASK_INDENT}{detail}"),
            Style::new().fg(COLOR_ERROR),
        )]));
    }

    // Render children
    if is_group {
        let child_prefix = format!("{prefix}{TASK_INDENT}");
        for child in &task.children {
            render_task_item(lines, child, &child_prefix, false, now, frame_index);
        }
    }
}

fn task_icon(state: &TaskState, frame_index: usize) -> &'static str {
    match state {
        TaskState::Pending => "○",
        TaskState::Running { .. } => TASK_SPINNER_TICKS[frame_index % TASK_SPINNER_TICKS.len()],
        TaskState::Succeeded { .. } => "✔",
        TaskState::Failed { .. } => "✘",
        TaskState::Cancelled { .. } => "⊘",
    }
}

fn pending_task_label(label: &str, state: &TaskState) -> String {
    match state {
        TaskState::Pending => format!("{label}..."),
        TaskState::Running { .. } | TaskState::Cancelled { .. } => format!("{label}…"),
        _ => label.to_string(),
    }
}

fn format_detail_suffix(task: &TaskItemState, now: Instant) -> String {
    // For failed tasks, detail goes on the error line below, not inline
    let detail = if matches!(task.state, TaskState::Failed { .. }) {
        None
    } else {
        task.detail
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
    };

    let elapsed = match &task.state {
        TaskState::Pending => None,
        TaskState::Running { started_at } => {
            let value = output::format_elapsed(now.saturating_duration_since(*started_at));
            (!value.is_empty()).then_some(value)
        }
        TaskState::Succeeded { elapsed }
        | TaskState::Failed { elapsed }
        | TaskState::Cancelled { elapsed } => elapsed.and_then(|e| {
            let value = output::format_elapsed_always(e);
            (!value.is_empty()).then_some(value)
        }),
    };

    match (elapsed, detail) {
        (Some(e), Some(d)) => format!("{e}, {d}"),
        (None, Some(d)) => d.to_string(),
        (Some(e), None) => e,
        (None, None) => String::new(),
    }
}

fn task_line_styles(state: &TaskState, is_group_like: bool) -> (Style, Style, Style) {
    let muted = Style::new().add_modifier(Modifier::DIM);
    let accent = Style::new().fg(COLOR_ACCENT);
    let success = Style::new().fg(COLOR_SUCCESS);
    let error = Style::new().fg(COLOR_ERROR);
    let normal = Style::new();

    match state {
        TaskState::Pending => (muted, muted, muted),
        TaskState::Failed { .. } => (error, if is_group_like { accent } else { normal }, muted),
        TaskState::Cancelled { elapsed } => (
            muted,
            if is_group_like && elapsed.is_some() {
                accent
            } else {
                muted
            },
            muted,
        ),
        TaskState::Succeeded { .. } => {
            (success, if is_group_like { accent } else { normal }, muted)
        }
        TaskState::Running { .. } if is_group_like => (accent, accent, muted),
        TaskState::Running { .. } => (normal, normal, muted),
    }
}

const PROGRESS_BAR_WIDTH: usize = 16;

fn render_block_bar_spans(spans: &mut Vec<Span<'static>>, fraction: f64) {
    let f = fraction.clamp(0.0, 1.0);
    let filled = (f * PROGRESS_BAR_WIDTH as f64).round() as usize;
    let empty = PROGRESS_BAR_WIDTH.saturating_sub(filled);

    // Gradient from left color → right color across the full bar width.
    const LEFT: (u8, u8, u8) = (110, 170, 220); // deeper blue
    const RIGHT: (u8, u8, u8) = (155, 217, 179); // brand green

    for i in 0..filled {
        let t = if PROGRESS_BAR_WIDTH <= 1 {
            0.0
        } else {
            i as f64 / (PROGRESS_BAR_WIDTH - 1) as f64
        };
        let r = (LEFT.0 as f64 + (RIGHT.0 as f64 - LEFT.0 as f64) * t) as u8;
        let g = (LEFT.1 as f64 + (RIGHT.1 as f64 - LEFT.1 as f64) * t) as u8;
        let b = (LEFT.2 as f64 + (RIGHT.2 as f64 - LEFT.2 as f64) * t) as u8;
        spans.push(Span::styled("█", Style::new().fg(Color::Rgb(r, g, b))));
    }
    if empty > 0 {
        spans.push(Span::styled(
            "░".repeat(empty),
            Style::new().add_modifier(Modifier::DIM),
        ));
    }
}

// ── Plain text rendering (for tests and CI) ─────────────────────────────

#[allow(dead_code)] // used by deploy/upgrade test assertions
pub fn render_plain_lines(tree: &[TreeNode]) -> Vec<String> {
    let now = Instant::now();
    let mut lines = Vec::new();
    for node in tree {
        match node {
            TreeNode::Task(task) => {
                render_task_item_plain(&mut lines, task, "", now);
            }
            TreeNode::AccentTask(task) => {
                render_task_item_plain(&mut lines, task, "", now);
            }
            TreeNode::Text { text, .. } => {
                lines.push(text.clone());
            }
            TreeNode::Spacer => {
                lines.push(String::new());
            }
        }
    }
    lines
}

fn render_task_item_plain(
    lines: &mut Vec<String>,
    task: &TaskItemState,
    prefix: &str,
    now: Instant,
) {
    let is_group = !task.children.is_empty();
    let icon = task_icon(&task.state, 0);
    let label = pending_task_label(&task.label, &task.state);
    let detail_suffix = format_detail_suffix(task, now);

    if detail_suffix.is_empty() {
        lines.push(format!("{prefix}{icon} {label}"));
    } else {
        lines.push(format!("{prefix}{icon} {label} {detail_suffix}"));
    }

    // Error detail line
    if matches!(task.state, TaskState::Failed { .. })
        && let Some(detail) = task
            .detail
            .as_deref()
            .map(str::trim)
            .filter(|d| !d.is_empty())
    {
        lines.push(format!("{prefix}{TASK_INDENT}{detail}"));
    }

    if is_group {
        let child_prefix = format!("{prefix}{TASK_INDENT}");
        for child in &task.children {
            render_task_item_plain(lines, child, &child_prefix, now);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_plain_task_states() {
        let _now = Instant::now();
        let tree = vec![TreeNode::Task(TaskItemState {
            id: "group".into(),
            label: "Checks".into(),
            state: TaskState::Pending,
            detail: None,
            progress: None,
            children: vec![
                TaskItemState {
                    id: "a".into(),
                    label: "prod-a".into(),
                    state: TaskState::Succeeded {
                        elapsed: Some(Duration::from_secs(2)),
                    },
                    detail: None,
                    progress: None,
                    children: vec![],
                },
                TaskItemState {
                    id: "b".into(),
                    label: "prod-b".into(),
                    state: TaskState::Failed {
                        elapsed: Some(Duration::from_secs(1)),
                    },
                    detail: Some("boom".into()),
                    progress: None,
                    children: vec![],
                },
                TaskItemState {
                    id: "c".into(),
                    label: "prod-c".into(),
                    state: TaskState::Cancelled { elapsed: None },
                    detail: Some("Skipped".into()),
                    progress: None,
                    children: vec![],
                },
            ],
        })];

        let lines = render_plain_lines(&tree);
        assert_eq!(lines[0], "○ Checks...");
        assert_eq!(lines[1], "  ✔ prod-a 2.0s");
        assert_eq!(lines[2], "  ✘ prod-b 1.0s");
        assert_eq!(lines[3], "    boom");
        assert_eq!(lines[4], "  ⊘ prod-c… Skipped");
    }

    #[test]
    fn render_plain_accent_task() {
        let tree = vec![
            TreeNode::AccentTask(TaskItemState {
                id: "build".into(),
                label: "Built".into(),
                state: TaskState::Succeeded {
                    elapsed: Some(Duration::from_millis(3400)),
                },
                detail: Some("1.04 MB".into()),
                progress: None,
                children: vec![],
            }),
            TreeNode::Spacer,
        ];

        let lines = render_plain_lines(&tree);
        assert_eq!(lines[0], "✔ Built 3.4s, 1.04 MB");
        assert_eq!(lines[1], "");
    }

    #[test]
    fn append_interrupt_message_adds_blank_line_and_error_text() {
        let mut tree = vec![TreeNode::Task(TaskItemState::pending(
            "deploy",
            "Deploying",
        ))];
        append_interrupt_message(&mut tree, "Operation cancelled");

        let lines = render_plain_lines(&tree);
        assert_eq!(
            lines,
            vec![
                "○ Deploying...".to_string(),
                String::new(),
                "Operation cancelled".to_string()
            ]
        );
    }

    #[test]
    fn append_interrupt_message_cancels_running_tasks() {
        let mut tree = vec![TreeNode::Task(TaskItemState {
            id: "deploy".into(),
            label: "Deploying".into(),
            state: TaskState::Running {
                started_at: Instant::now(),
            },
            detail: None,
            progress: None,
            children: vec![
                TaskItemState {
                    id: "a".into(),
                    label: "Connected".into(),
                    state: TaskState::Succeeded {
                        elapsed: Some(Duration::from_secs(1)),
                    },
                    detail: None,
                    progress: None,
                    children: vec![],
                },
                TaskItemState {
                    id: "b".into(),
                    label: "Starting".into(),
                    state: TaskState::Running {
                        started_at: Instant::now(),
                    },
                    detail: None,
                    progress: None,
                    children: vec![],
                },
            ],
        })];
        append_interrupt_message(&mut tree, "Operation cancelled");

        let lines = render_plain_lines(&tree);
        // Parent and running child should be cancelled
        assert!(lines[0].starts_with("⊘ Deploying…"));
        assert!(lines[1].starts_with("  ✔ Connected"));
        assert!(lines[2].starts_with("  ⊘ Starting…"));
        assert_eq!(lines[3], "");
        assert_eq!(lines[4], "Operation cancelled");
    }

    #[test]
    fn task_item_find_and_find_mut() {
        let mut root = TaskItemState::pending("root", "Root").with_children(vec![
            TaskItemState::pending("child-a", "A"),
            TaskItemState::pending("child-b", "B"),
        ]);

        assert!(root.find("child-a").is_some());
        assert!(root.find("missing").is_none());

        let child = root.find_mut("child-b").unwrap();
        child.label = "Updated".into();
        assert_eq!(root.find("child-b").unwrap().label, "Updated");
    }
}
