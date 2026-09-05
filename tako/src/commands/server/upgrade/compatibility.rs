use crate::ssh::SshClient;

use super::{SERVER_BINARY_PATH, first_non_empty_line};

fn remote_upgrade_reload_marker_path(data_dir: &str) -> String {
    format!(
        "{}/{}",
        data_dir.trim_end_matches('/'),
        tako_core::UPGRADE_RELOAD_MARKER_FILE
    )
}

pub(super) fn remote_prepare_upgrade_reload_command(data_dir: &str, owner: &str) -> String {
    let marker = crate::shell::shell_single_quote(&remote_upgrade_reload_marker_path(data_dir));
    let owner = crate::shell::shell_single_quote(owner);
    format!("umask 077; printf '%s\\n' {owner} > {marker}; chmod 0644 {marker}")
}

pub(super) fn remote_cleanup_upgrade_reload_command(data_dir: &str) -> String {
    let marker = crate::shell::shell_single_quote(&remote_upgrade_reload_marker_path(data_dir));
    format!("rm -f {marker}")
}

pub(super) fn remote_protocol_compatibility_command(data_dir: &str, force: bool) -> String {
    let data_dir = crate::shell::shell_single_quote(data_dir);
    let force_arg = if force {
        " --allow-incompatible-protocol"
    } else {
        ""
    };
    format!(
        "{SERVER_BINARY_PATH} --check-protocol-compatibility --data-dir {data_dir} --expected-protocol-version {}{force_arg}",
        tako_core::PROTOCOL_VERSION
    )
}

pub(super) async fn check_remote_protocol_compatibility(
    ssh: &mut SshClient,
    data_dir: &str,
    force: bool,
) -> Result<(), String> {
    let output = ssh
        .exec(&remote_protocol_compatibility_command(data_dir, force))
        .await
        .map_err(|error| format!("Protocol compatibility check failed: {error}"))?;
    if output.success() {
        return Ok(());
    }
    let combined = output.combined();
    let message = first_non_empty_line(combined.trim())
        .unwrap_or("candidate server rejected the active releases");
    Err(format!("Protocol compatibility check failed: {message}"))
}
