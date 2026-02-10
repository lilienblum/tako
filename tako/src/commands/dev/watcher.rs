//! Config watcher for `tako dev`.
//!
//! Watches `tako.toml` changes in the project root.

use notify::RecursiveMode;
use notify_debouncer_mini::new_debouncer;
use std::path::PathBuf;
use std::sync::mpsc as std_mpsc;
use std::time::Duration;
use tokio::sync::mpsc;

/// Handle that keeps the watcher alive
pub struct WatcherHandle {
    _debouncer: notify_debouncer_mini::Debouncer<notify::RecommendedWatcher>,
    _thread: std::thread::JoinHandle<()>,
}

/// Watches `tako.toml` for changes (create/update/delete) in a project directory.
pub struct ConfigWatcher {
    project_dir: PathBuf,
    changed_tx: mpsc::Sender<()>,
}

impl ConfigWatcher {
    pub fn new(
        project_dir: PathBuf,
        changed_tx: mpsc::Sender<()>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            project_dir,
            changed_tx,
        })
    }

    pub fn start(self) -> Result<WatcherHandle, Box<dyn std::error::Error>> {
        let (tx, rx) = std_mpsc::channel();
        let mut debouncer = new_debouncer(Duration::from_millis(150), tx)?;

        // Watch just the project root (non-recursive) so we can detect `tako.toml` create/delete.
        debouncer
            .watcher()
            .watch(&self.project_dir, RecursiveMode::NonRecursive)?;

        let changed_tx = self.changed_tx.clone();
        let project_dir = self.project_dir.clone();
        let watched = self.project_dir.join("tako.toml");
        let handle = std::thread::spawn(move || {
            for result in rx {
                match result {
                    Ok(events) => {
                        for event in events {
                            if event.path == watched {
                                let _ = changed_tx.blocking_send(());
                            } else {
                                // Some platforms report directory-level events; treat any event in
                                // the project root as a hint and re-check if tako.toml exists.
                                if event.path == project_dir {
                                    let _ = changed_tx.blocking_send(());
                                }
                            }
                        }
                    }
                    Err(e) => {
                        eprintln!("Watch error: {:?}", e);
                    }
                }
            }
        });

        Ok(WatcherHandle {
            _debouncer: debouncer,
            _thread: handle,
        })
    }
}
