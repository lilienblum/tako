use std::path::{Path, PathBuf};

/// If `tako-server` is being run from a path under a `target/` directory, return that
/// `target/` directory path.
pub fn target_dir_from_exe(exe_path: &Path) -> Option<PathBuf> {
    let mut cur = exe_path;
    loop {
        if cur.file_name().is_some_and(|n| n == "target") {
            return Some(cur.to_path_buf());
        }
        cur = cur.parent()?;
    }
}

/// If `tako-server` is being run from a path under a `target/` directory, return the
/// repo root directory (the parent of `target/`).
pub fn repo_root_from_exe(exe_path: &Path) -> Option<PathBuf> {
    target_dir_from_exe(exe_path)?
        .parent()
        .map(|p| p.to_path_buf())
}

/// Default unix socket path for debug builds when running from a source checkout.
///
/// Example: `{repo}/local-dev/tako-server/tmp/tako.sock`
pub fn debug_default_socket_from_exe(exe_path: &Path) -> Option<PathBuf> {
    repo_root_from_exe(exe_path).map(|root| {
        root.join("local-dev")
            .join("tako-server")
            .join("tmp")
            .join("tako.sock")
    })
}

/// Default data dir for debug builds when running from a source checkout.
///
/// Example: `{repo}/local-dev/tako-server/data`
pub fn debug_default_data_dir_from_exe(exe_path: &Path) -> Option<PathBuf> {
    repo_root_from_exe(exe_path).map(|root| root.join("local-dev").join("tako-server").join("data"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repo_root_from_exe_finds_repo_root() {
        let exe = PathBuf::from("/Users/me/proj/target/debug/tako-server");
        assert_eq!(
            repo_root_from_exe(&exe).as_deref(),
            Some(Path::new("/Users/me/proj"))
        );
    }

    #[test]
    fn debug_default_socket_is_under_local_dev_tmp() {
        let exe = PathBuf::from("/Users/me/proj/target/debug/tako-server");
        assert_eq!(
            debug_default_socket_from_exe(&exe).as_deref(),
            Some(Path::new(
                "/Users/me/proj/local-dev/tako-server/tmp/tako.sock"
            ))
        );
    }
}
