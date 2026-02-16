use std::path::{Path, PathBuf};
#[cfg(test)]
use std::sync::{Mutex, MutexGuard, OnceLock};

/// Get Tako's global home directory.
///
/// - In debug builds, prefer `{repo}/debug/.tako` when running from a source checkout.
/// - In release builds, default to `~/.tako`.
pub fn tako_home_dir() -> Result<PathBuf, std::io::Error> {
    if let Ok(v) = std::env::var("TAKO_HOME")
        && !v.trim().is_empty()
    {
        return Ok(PathBuf::from(v));
    }

    if cfg!(debug_assertions)
        && let Ok(exe) = std::env::current_exe()
        && let Some(dev_home) = dev_tako_home_from_exe(&exe)
    {
        return Ok(dev_home);
    }

    let home = dirs::home_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "Could not determine home directory",
        )
    })?;

    Ok(home.join(".tako"))
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
/// Example: `{repo}/debug/.tako`
pub fn dev_tako_home_from_exe(exe_path: &Path) -> Option<PathBuf> {
    repo_root_from_exe(exe_path).map(|root| root.join("debug").join(".tako"))
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
    fn tako_home_dir_respects_env_override() {
        let _lock = test_tako_home_env_lock();
        let previous = std::env::var_os("TAKO_HOME");
        let temp = TempDir::new().unwrap();
        unsafe {
            std::env::set_var("TAKO_HOME", temp.path());
        }
        let got = tako_home_dir().unwrap();
        match previous {
            Some(value) => unsafe { std::env::set_var("TAKO_HOME", value) },
            None => unsafe { std::env::remove_var("TAKO_HOME") },
        }
        assert_eq!(got, temp.path());
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
            Some(Path::new("/Users/me/proj/debug/.tako"))
        );
    }
}
