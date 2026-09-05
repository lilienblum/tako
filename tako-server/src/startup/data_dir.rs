use std::path::Path;

/// Permissions for the tako data directory (typically `/opt/tako`).
///
/// The installer assigns the shared `tako-app` traversal group. Individual
/// app directories have private groups; control-plane directories are 0700.
#[cfg(unix)]
const DATA_DIR_MODE: u32 = 0o710;

/// Create the tako data directory (idempotent) and set its permissions
/// so per-app sandbox users can traverse into release and runtime
/// subdirectories. See [`DATA_DIR_MODE`] for rationale.
#[cfg(unix)]
pub(crate) fn prepare_data_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(DATA_DIR_MODE))?;
    for name in ["certs", "acme"] {
        let directory = path.join(name);
        std::fs::create_dir_all(&directory)?;
        std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn prepare_data_dir(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)
}
