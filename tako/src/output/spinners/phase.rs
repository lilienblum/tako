use std::time::{Duration, Instant};

use indicatif::ProgressBar;

use super::{finish_spinner_ok, phase_spinner_style, phase_spinner_style_indented};
use crate::output::cursor::{
    clear_active_progress_bar, hide_cursor, register_active_progress_bar, show_cursor,
};
use crate::output::{
    INDENT, PHASE_PB, error_block, format_elapsed_inline, is_interactive, is_pretty, muted_elapsed,
    theme_fg, theme_muted, theme_success,
};

/// A spinner for major phases (Build, Deploy). Shows elapsed time after 1s.
/// Inner output is NOT suppressed — it flows normally above the spinner.
///
/// In verbose/CI mode: silent — no spinner, no tracing. The caller's
/// `output::timed()` owns phase tracing.
pub struct PhaseSpinner {
    pb: Option<ProgressBar>,
    start: Instant,
    finished: bool,
    verbose: bool,
    _elapsed_task: Option<tokio::task::JoinHandle<()>>,
}

impl PhaseSpinner {
    pub fn start(message: &str) -> Self {
        Self::new(message, false)
    }

    /// Start an indented phase spinner (prefixed with INDENT).
    pub fn start_indented(message: &str) -> Self {
        Self::new(message, true)
    }

    fn new(message: &str, indented: bool) -> Self {
        let verbose = !is_pretty();

        if verbose {
            return Self {
                pb: None,
                start: Instant::now(),
                finished: false,
                verbose: true,
                _elapsed_task: None,
            };
        }

        let style = if indented {
            phase_spinner_style_indented()
        } else {
            phase_spinner_style()
        };
        let pb = if is_interactive() {
            let pb = ProgressBar::new_spinner();
            pb.set_style(style);
            pb.set_message(message.to_string());
            pb.enable_steady_tick(Duration::from_millis(80));
            hide_cursor();
            register_active_progress_bar(&pb);
            Some(pb)
        } else {
            None
        };

        // Spawn a task that updates the message with elapsed time every second.
        let elapsed_task = pb.as_ref().map(|pb| {
            let pb = pb.clone();
            let base = message.to_string();
            let start = Instant::now();
            tokio::spawn(async move {
                // Wait 1s before showing elapsed at all.
                tokio::time::sleep(Duration::from_secs(1)).await;
                loop {
                    let elapsed = start.elapsed();
                    let time = format_elapsed_inline(elapsed);
                    pb.set_message(format!("{base} {}", theme_muted(&time)));
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            })
        });

        // Register the active phase spinner so all output routes through it.
        if let Some(ref pb) = pb {
            *PHASE_PB.lock().unwrap() = Some(pb.clone());
        }

        Self {
            pb,
            start: Instant::now(),
            finished: false,
            verbose: false,
            _elapsed_task: elapsed_task,
        }
    }

    pub fn pb(&self) -> Option<&ProgressBar> {
        self.pb.as_ref()
    }

    pub fn finish(mut self, success_msg: &str) {
        self.abort_elapsed_task();
        self.clear_global();
        if self.verbose {
            // In verbose mode the start message already persists — no result line needed.
        } else if let Some(ref pb) = self.pb {
            clear_active_progress_bar();
            finish_spinner_ok(pb, success_msg, self.start.elapsed());
        }
        self.finished = true;
    }

    /// Finish indented spinner with success: `  ✔ message (elapsed)`
    pub fn finish_ok_indented(mut self, success_msg: &str) {
        self.abort_elapsed_task();
        self.clear_global();
        if self.verbose {
            // In verbose mode the start message already persists — no result line needed.
        } else if let Some(ref pb) = self.pb {
            clear_active_progress_bar();
            pb.finish_and_clear();
            show_cursor();
            let check = theme_success("✔");
            let time = muted_elapsed(self.start.elapsed());
            if time.is_empty() {
                eprintln!("{INDENT}{check} {}", theme_fg(success_msg));
            } else {
                eprintln!("{INDENT}{check} {}  {time}", theme_fg(success_msg));
            }
        }
        self.finished = true;
    }

    /// Finish indented spinner with error — clears the spinner, then shows an error block.
    pub fn finish_err_indented(mut self, detail: &str) {
        self.abort_elapsed_task();
        self.clear_global();
        if self.verbose {
            tracing::error!("{}", detail);
        } else if let Some(ref pb) = self.pb {
            clear_active_progress_bar();
            pb.finish_and_clear();
            show_cursor();
            error_block(detail);
        }
        self.finished = true;
    }

    fn abort_elapsed_task(&mut self) {
        if let Some(handle) = self._elapsed_task.take() {
            handle.abort();
        }
    }

    fn clear_global(&self) {
        *PHASE_PB.lock().unwrap() = None;
    }
}

impl Drop for PhaseSpinner {
    fn drop(&mut self) {
        self.abort_elapsed_task();
        self.clear_global();
        if !self.finished
            && let Some(ref pb) = self.pb
        {
            clear_active_progress_bar();
            pb.finish_and_clear();
            show_cursor();
        }
    }
}
