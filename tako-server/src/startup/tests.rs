use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn prepare_data_dir_creates_dir_with_group_traverse_mode() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("tako-data");

    prepare_data_dir(&dir).expect("prepare_data_dir");

    assert!(dir.is_dir());
    assert_eq!(
        mode_of(&dir),
        0o710,
        "data dir must grant group traverse so tako-app can descend into \
         runtimes/ and releases/ to exec app binaries; 0o700 triggers \
         ENOENT on execve because the kernel denies directory traversal"
    );
}

#[test]
fn prepare_data_dir_sets_mode_on_existing_dir() {
    // Existing directories need the same traversal permission as new ones.
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("tako-data");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();

    prepare_data_dir(&dir).expect("prepare_data_dir");

    assert_eq!(mode_of(&dir), 0o710);
}

#[test]
fn prepare_data_dir_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("tako-data");

    prepare_data_dir(&dir).expect("prepare_data_dir first call");
    prepare_data_dir(&dir).expect("prepare_data_dir second call");

    assert_eq!(mode_of(&dir), 0o710);
}

#[test]
fn prepare_data_dir_keeps_certificate_and_account_directories_private() {
    let tmp = tempfile::tempdir().unwrap();
    prepare_data_dir(tmp.path()).unwrap();
    assert_eq!(mode_of(&tmp.path().join("certs")), 0o700);
    assert_eq!(mode_of(&tmp.path().join("acme")), 0o700);
}

#[test]
fn upgrade_reload_marker_is_consumed() {
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join(tako_core::UPGRADE_RELOAD_MARKER_FILE);
    std::fs::write(&marker, "controller-a\n").unwrap();

    let owner = take_upgrade_reload_owner(tmp.path()).unwrap();

    assert_eq!(owner.as_deref(), Some("controller-a"));
    assert!(!marker.exists());
}
