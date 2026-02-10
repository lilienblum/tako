use std::path::Path;

/// Best-effort cleanup for stale app unix socket files.
///
/// App sockets are expected to be named like:
/// `tako-app-{app-name}-{pid}.sock`
///
/// This function removes sockets whose PID no longer exists.
pub fn cleanup_stale_app_sockets(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in rd.flatten() {
        let path = entry.path();
        if !is_candidate_socket(&path) {
            continue;
        }

        let Some(pid) = pid_from_socket_name(&path) else {
            continue;
        };

        if pid_exists(pid) {
            continue;
        }

        let _ = std::fs::remove_file(&path);
    }
}

fn is_candidate_socket(path: &Path) -> bool {
    if path.extension().and_then(|e| e.to_str()) != Some("sock") {
        return false;
    }
    let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
        return false;
    };
    name.starts_with("tako-app-")
}

fn pid_from_socket_name(path: &Path) -> Option<u32> {
    let name = path.file_name()?.to_str()?;
    // tako-app-{app}-{pid}.sock
    let without_ext = name.strip_suffix(".sock")?;
    let pid_str = without_ext.rsplit('-').next()?;
    pid_str.parse::<u32>().ok()
}

fn pid_exists(pid: u32) -> bool {
    // kill(pid, 0) checks for existence.
    // Returns 0 if process exists and we have permission.
    // Returns -1/EPERM if process exists but we lack permission.
    // Returns -1/ESRCH if process doesn't exist.
    unsafe {
        let r = libc::kill(pid as i32, 0);
        if r == 0 {
            return true;
        }
        matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(code) if code == libc::EPERM
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn parses_pid_from_socket_name() {
        let p = PathBuf::from("/var/run/tako-app-my-app-12345.sock");
        assert_eq!(pid_from_socket_name(&p), Some(12345));
    }
}
