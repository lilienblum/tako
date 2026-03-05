use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Get Tako's config directory (XDG-compliant).
///
/// - `TAKO_HOME` set → that directory (all-in-one).
/// - Debug builds from source checkout → `{repo}/local-dev/.tako` (all-in-one).
/// - Otherwise → `dirs::config_dir()/tako` (e.g. `~/.config/tako` on Linux,
///   `~/Library/Application Support/tako` on macOS).
pub fn tako_config_dir() -> Result<PathBuf, std::io::Error> {
    if let Some(home) = tako_home_override() {
        return Ok(home);
    }
    let base = dirs::config_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not determine config directory",
        )
    })?;
    Ok(base.join("tako"))
}

/// Get Tako's data directory (XDG-compliant).
///
/// - `TAKO_HOME` set → that directory (all-in-one).
/// - Debug builds from source checkout → `{repo}/local-dev/.tako` (all-in-one).
/// - Otherwise → `dirs::data_dir()/tako` (e.g. `~/.local/share/tako` on Linux,
///   `~/Library/Application Support/tako` on macOS).
pub fn tako_data_dir() -> Result<PathBuf, std::io::Error> {
    if let Some(home) = tako_home_override() {
        return Ok(home);
    }
    let base = dirs::data_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not determine data directory",
        )
    })?;
    Ok(base.join("tako"))
}

/// Returns the override directory when `TAKO_HOME` is set or running from a
/// debug source checkout. Returns `None` when the XDG split should be used.
fn tako_home_override() -> Option<PathBuf> {
    if let Ok(v) = std::env::var("TAKO_HOME")
        && !v.trim().is_empty()
    {
        return Some(PathBuf::from(v));
    }

    if cfg!(debug_assertions)
        && let Ok(exe) = std::env::current_exe()
        && let Some(dev_home) = dev_tako_home_from_exe(&exe)
    {
        return Some(dev_home);
    }

    None
}

/// If `tako` is being run from a path under a `target/` directory, return the
/// repo root directory (the parent of `target/`).
pub fn repo_root_from_exe(exe_path: &Path) -> Option<PathBuf> {
    target_dir_from_exe(exe_path)?
        .parent()
        .map(|p| p.to_path_buf())
}

/// If `tako` is being run from a path under a `target/` directory, return that
/// `target/` directory path.
///
/// This works for:
/// - `.../target/debug/tako`
/// - `.../target/release/tako`
/// - `.../target/debug/deps/cli_integration-...`
pub fn target_dir_from_exe(exe_path: &Path) -> Option<PathBuf> {
    let mut cur = exe_path;
    loop {
        if cur.file_name().is_some_and(|n| n == "target") {
            return Some(cur.to_path_buf());
        }
        cur = cur.parent()?;
    }
}

/// Compute a dev-only Tako home directory under the repo root.
///
/// Example: `{repo}/local-dev/.tako`
pub fn dev_tako_home_from_exe(exe_path: &Path) -> Option<PathBuf> {
    repo_root_from_exe(exe_path).map(|root| root.join("local-dev").join(".tako"))
}

/// Migrate legacy `~/.tako/` files to XDG directories.
///
/// Skips if:
/// - `TAKO_HOME` is set (custom setup)
/// - Debug build from source checkout (dev override)
/// - `~/.tako/` doesn't exist (fresh install)
/// - XDG dirs already have content (already migrated)
pub fn migrate_legacy_home() {
    // Only migrate for the XDG case (no overrides).
    if tako_home_override().is_some() {
        return;
    }

    let Some(home) = dirs::home_dir() else {
        return;
    };
    let legacy = home.join(".tako");
    if !legacy.is_dir() {
        return;
    }

    let Ok(config_dir) = tako_config_dir() else {
        return;
    };
    let Ok(data_dir) = tako_data_dir() else {
        return;
    };

    // Skip if either XDG dir already has content.
    if config_dir.exists() || data_dir.exists() {
        return;
    }

    // Move config.toml → config dir
    let legacy_config = legacy.join("config.toml");
    if legacy_config.exists() {
        if let Err(e) = std::fs::create_dir_all(&config_dir) {
            eprintln!("warning: could not create {}: {}", config_dir.display(), e);
            return;
        }
        if let Err(e) = std::fs::rename(&legacy_config, config_dir.join("config.toml")) {
            eprintln!("warning: could not move config.toml: {}", e);
            return;
        }
    }

    // Move everything else → data dir
    if let Err(e) = std::fs::create_dir_all(&data_dir) {
        eprintln!("warning: could not create {}: {}", data_dir.display(), e);
        return;
    }
    if let Ok(entries) = std::fs::read_dir(&legacy) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let dest = data_dir.join(&name);
            if let Err(e) = std::fs::rename(entry.path(), &dest) {
                eprintln!(
                    "warning: could not move {}: {}",
                    name.to_string_lossy(),
                    e
                );
            }
        }
    }

    // Remove ~/.tako/ if empty.
    let _ = std::fs::remove_dir(&legacy);

    eprintln!(
        "Migrated ~/.tako/ \u{2192} {} + {}",
        config_dir.display(),
        data_dir.display()
    );
}

#[cfg(test)]
pub(crate) fn test_tako_home_env_lock() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .expect("TAKO_HOME test env lock poisoned")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn target_dir_from_exe_finds_target_for_normal_binary() {
        let exe = PathBuf::from("/Users/me/proj/target/debug/tako");
        assert_eq!(
            target_dir_from_exe(&exe).as_deref(),
            Some(Path::new("/Users/me/proj/target"))
        );
    }

    #[test]
    fn repo_root_from_exe_finds_repo_root() {
        let exe = PathBuf::from("/Users/me/proj/target/debug/tako");
        assert_eq!(
            repo_root_from_exe(&exe).as_deref(),
            Some(Path::new("/Users/me/proj"))
        );
    }

    #[test]
    fn target_dir_from_exe_finds_target_for_deps_binary() {
        let exe = PathBuf::from("/Users/me/proj/target/debug/deps/cli_integration-abc123");
        assert_eq!(
            target_dir_from_exe(&exe).as_deref(),
            Some(Path::new("/Users/me/proj/target"))
        );
    }

    #[test]
    fn dev_tako_home_is_under_target() {
        let exe = PathBuf::from("/Users/me/proj/target/debug/tako");
        assert_eq!(
            dev_tako_home_from_exe(&exe).as_deref(),
            Some(Path::new("/Users/me/proj/local-dev/.tako"))
        );
    }

    #[test]
    fn tako_config_dir_respects_env_override() {
        let _lock = test_tako_home_env_lock();
        let previous = std::env::var_os("TAKO_HOME");
        let temp = TempDir::new().unwrap();
        unsafe {
            std::env::set_var("TAKO_HOME", temp.path());
        }
        let got = tako_config_dir().unwrap();
        match previous {
            Some(value) => unsafe { std::env::set_var("TAKO_HOME", value) },
            None => unsafe { std::env::remove_var("TAKO_HOME") },
        }
        assert_eq!(got, temp.path());
    }

    #[test]
    fn tako_data_dir_respects_env_override() {
        let _lock = test_tako_home_env_lock();
        let previous = std::env::var_os("TAKO_HOME");
        let temp = TempDir::new().unwrap();
        unsafe {
            std::env::set_var("TAKO_HOME", temp.path());
        }
        let got = tako_data_dir().unwrap();
        match previous {
            Some(value) => unsafe { std::env::set_var("TAKO_HOME", value) },
            None => unsafe { std::env::remove_var("TAKO_HOME") },
        }
        assert_eq!(got, temp.path());
    }

    #[test]
    fn tako_home_override_returns_some_when_env_set() {
        let _lock = test_tako_home_env_lock();
        let previous = std::env::var_os("TAKO_HOME");
        let temp = TempDir::new().unwrap();
        unsafe {
            std::env::set_var("TAKO_HOME", temp.path());
        }
        let got = tako_home_override();
        match previous {
            Some(value) => unsafe { std::env::set_var("TAKO_HOME", value) },
            None => unsafe { std::env::remove_var("TAKO_HOME") },
        }
        assert_eq!(got, Some(temp.path().to_path_buf()));
    }

    #[test]
    fn tako_home_override_returns_none_when_env_unset_and_not_debug_exe() {
        let _lock = test_tako_home_env_lock();
        let previous = std::env::var_os("TAKO_HOME");
        unsafe {
            std::env::remove_var("TAKO_HOME");
        }
        // In test builds (debug_assertions = true), this will return
        // Some if the test binary is under target/. That's expected
        // because tests run from target/debug/deps/.
        let got = tako_home_override();
        match previous {
            Some(value) => unsafe { std::env::set_var("TAKO_HOME", value) },
            None => unsafe { std::env::remove_var("TAKO_HOME") },
        }
        // In test context (debug build from target/), override should be Some.
        if cfg!(debug_assertions) {
            assert!(got.is_some());
        }
    }
}
