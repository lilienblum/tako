use super::*;
use std::fs;
use tempfile::TempDir;

#[cfg(unix)]
#[test]
fn test_compute_source_hash_supports_directory_symlinks() {
    use std::os::unix::fs as unix_fs;

    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    fs::create_dir_all(source.join("sdk")).unwrap();
    fs::create_dir_all(source.join("app")).unwrap();
    fs::write(source.join("sdk/index.js"), "ok").unwrap();
    unix_fs::symlink("../sdk", source.join("app/linked-sdk")).unwrap();

    let executor = BuildExecutor::new(&source);
    let hash = executor.compute_source_hash(&source).unwrap();
    assert!(!hash.is_empty());
}

#[test]
fn test_compute_file_hash() {
    let temp = TempDir::new().unwrap();
    let file_path = temp.path().join("test.txt");
    fs::write(&file_path, "hello world").unwrap();

    let hash = compute_file_hash(&file_path).unwrap();
    // SHA256 of "hello world"
    assert_eq!(
        hash,
        "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
    );
}

#[test]
fn test_compute_source_hash_respects_gitignore_and_forced_exclusions() {
    let temp = TempDir::new().unwrap();
    let source = temp.path().join("source");
    fs::create_dir_all(source.join("src")).unwrap();
    fs::create_dir_all(source.join("dist")).unwrap();
    fs::create_dir_all(source.join(".git")).unwrap();
    fs::create_dir_all(source.join("node_modules/pkg")).unwrap();
    fs::create_dir_all(source.join("target/debug")).unwrap();

    fs::write(source.join(".gitignore"), "dist/\n").unwrap();
    fs::write(source.join("src/main.ts"), "main-v1").unwrap();
    fs::write(source.join("dist/out.txt"), "out-v1").unwrap();
    fs::write(source.join(".env.production"), "secret-v1").unwrap();
    fs::write(source.join(".git/config"), "git-v1").unwrap();
    fs::write(source.join("node_modules/pkg/index.js"), "pkg-v1").unwrap();
    fs::write(source.join("target/debug/out.txt"), "out-v1").unwrap();

    let executor = BuildExecutor::new(&source);
    let hash1 = executor.compute_source_hash(&source).unwrap();

    // Changes to excluded files should not change the source hash.
    fs::write(source.join("dist/out.txt"), "out-v2").unwrap();
    fs::write(source.join(".env.production"), "secret-v2").unwrap();
    fs::write(source.join(".git/config"), "git-v2").unwrap();
    fs::write(source.join("node_modules/pkg/index.js"), "pkg-v2").unwrap();
    fs::write(source.join("target/debug/out.txt"), "out-v2").unwrap();
    let hash2 = executor.compute_source_hash(&source).unwrap();
    assert_eq!(hash1, hash2);

    // Changes to included files should change the source hash.
    fs::write(source.join("src/main.ts"), "main-v2").unwrap();
    let hash3 = executor.compute_source_hash(&source).unwrap();
    assert_ne!(hash2, hash3);
}

#[test]
fn test_generate_version_falls_back_when_git_commit_missing() {
    let temp = TempDir::new().unwrap();
    let executor = BuildExecutor::new(temp.path());

    let version = executor.generate_version(Some("abcdef123456")).unwrap();
    assert_eq!(version, "nogit_abcdef12");
}

#[test]
fn test_generate_version_falls_back_with_timestamp_when_no_hash() {
    let temp = TempDir::new().unwrap();
    let executor = BuildExecutor::new(temp.path());

    let version = executor.generate_version(None).unwrap();
    assert!(version.starts_with("nogit_"));
    assert!(version.len() > "nogit_".len());
}
