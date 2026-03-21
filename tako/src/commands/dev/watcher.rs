//! Config watcher for `tako dev`.
//!
//! Watches the selected config file and `.tako/secrets.json`.

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

/// Watches `tako.toml` and `.tako/secrets.json` for changes in a project directory.
pub struct ConfigWatcher {
    project_dir: PathBuf,
    config_path: PathBuf,
    changed_tx: mpsc::Sender<()>,
}

impl ConfigWatcher {
    pub fn new(
        project_dir: PathBuf,
        config_path: PathBuf,
        changed_tx: mpsc::Sender<()>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self {
            project_dir,
            config_path,
            changed_tx,
        })
    }

    pub fn start(self) -> Result<WatcherHandle, Box<dyn std::error::Error>> {
        let (tx, rx) = std_mpsc::channel();
        let mut debouncer = new_debouncer(Duration::from_millis(150), tx)?;

        // Watch the selected config file directly.
        debouncer
            .watcher()
            .watch(&self.config_path, RecursiveMode::NonRecursive)?;

        // Watch .tako/ directory for secrets.json changes.
        let tako_dir = self.project_dir.join(".tako");
        if tako_dir.is_dir() {
            let _ = debouncer
                .watcher()
                .watch(&tako_dir, RecursiveMode::NonRecursive);
        }

        let changed_tx = self.changed_tx.clone();
        let watched_config = self.config_path.clone();
        let watched_secrets = self.project_dir.join(".tako").join("secrets.json");
        let handle = std::thread::spawn(move || {
            for result in rx {
                match result {
                    Ok(events) => {
                        for event in events {
                            if event.path == watched_config || event.path == watched_secrets {
                                let _ = changed_tx.blocking_send(());
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
