use std::path::{Path, PathBuf};

/// Get Tako's global home directory.
///
/// Mirrors `tako/src/paths.rs`:
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

pub fn repo_root_from_exe(exe_path: &Path) -> Option<PathBuf> {
    target_dir_from_exe(exe_path)?
        .parent()
        .map(|p| p.to_path_buf())
}

pub fn target_dir_from_exe(exe_path: &Path) -> Option<PathBuf> {
    let mut cur = exe_path;
    loop {
        if cur.file_name().is_some_and(|n| n == "target") {
            return Some(cur.to_path_buf());
        }
        cur = cur.parent()?;
    }
}

pub fn dev_tako_home_from_exe(exe_path: &Path) -> Option<PathBuf> {
    repo_root_from_exe(exe_path).map(|root| root.join("debug").join(".tako"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn target_dir_from_exe_finds_target_for_normal_binary() {
        let exe = PathBuf::from("/Users/me/proj/target/debug/tako-dev-server");
        assert_eq!(
            target_dir_from_exe(&exe).as_deref(),
            Some(Path::new("/Users/me/proj/target"))
        );
    }

    #[test]
    fn repo_root_from_exe_finds_repo_root() {
        let exe = PathBuf::from("/Users/me/proj/target/debug/tako-dev-server");
        assert_eq!(
            repo_root_from_exe(&exe).as_deref(),
            Some(Path::new("/Users/me/proj"))
        );
    }

    #[test]
    fn dev_tako_home_is_under_debug() {
        let exe = PathBuf::from("/Users/me/proj/target/debug/tako-dev-server");
        assert_eq!(
            dev_tako_home_from_exe(&exe).as_deref(),
            Some(Path::new("/Users/me/proj/debug/.tako"))
        );
    }

    #[test]
    fn tako_home_dir_respects_env_override() {
        let temp = TempDir::new().unwrap();
        unsafe {
            std::env::set_var("TAKO_HOME", temp.path());
        }
        let got = tako_home_dir().unwrap();
        unsafe {
            std::env::remove_var("TAKO_HOME");
        }
        assert_eq!(got, temp.path());
    }
}
