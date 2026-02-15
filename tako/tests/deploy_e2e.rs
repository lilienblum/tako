use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use tempfile::TempDir;

fn workspace_root() -> PathBuf {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| manifest_dir.to_path_buf())
}

fn apply_coverage_env(cmd: &mut Command) {
    let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") else {
        return;
    };
    let profile = PathBuf::from(profile);
    if profile.is_absolute() {
        return;
    }
    let absolute = workspace_root().join(profile);
    if let Some(parent) = absolute.parent() {
        let _ = fs::create_dir_all(parent);
    }
    cmd.env("LLVM_PROFILE_FILE", absolute);
}

fn extract_version(output: &str) -> Option<String> {
    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("Version: ") {
            return Some(rest.trim().to_string());
        }
        if let Some(rest) = line.strip_prefix("OK Version: ") {
            return Some(rest.trim().to_string());
        }
        // Typical output includes leading indentation.
        if let Some(rest) = line.strip_prefix("Version:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

fn docker_ok() -> bool {
    Command::new("docker")
        .args(["version", "--format", "{{.Server.Version}}"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn docker_output(args: &[&str]) -> std::process::Output {
    Command::new("docker")
        .args(args)
        .output()
        .expect("docker command failed")
}

fn docker_stdout(args: &[&str]) -> String {
    let out = docker_output(args);
    assert!(out.status.success(), "docker {:?} failed", args);
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

struct DockerContainer {
    id: String,
}

impl Drop for DockerContainer {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rm", "-f", &self.id]).output();
    }
}

fn build_image(tag: &str) {
    let ctx = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("deploy-alpine");

    let out = Command::new("docker")
        .args(["build", "-t", tag, "."])
        .current_dir(&ctx)
        .output()
        .expect("docker build failed");
    assert!(
        out.status.success(),
        "docker build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn local_ssh_public_key() -> Option<String> {
    let home = dirs::home_dir()?;
    let ssh_dir = home.join(".ssh");
    let candidates = ["id_ed25519.pub", "id_rsa.pub", "id_ecdsa.pub", "id_dsa.pub"];

    for candidate in candidates {
        let key_path = ssh_dir.join(candidate);
        if key_path.exists() {
            let key = fs::read_to_string(key_path).ok()?;
            if !key.trim().is_empty() {
                return Some(key);
            }
        }
    }

    None
}

fn run_tako(args: &[&str], cwd: &Path, tako_home: &Path) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tako"));
    cmd.args(args)
        .current_dir(cwd)
        .env("TAKO_HOME", tako_home)
        // Point auto-install at the container-local artifact server.
        .env(
            "TAKO_SERVER_INSTALL_URL",
            "http://127.0.0.1:8000/tako-server",
        )
        .env(
            "TAKO_SERVER_INSTALL_SHA256",
            "cf6c075a3aa10b2e7f1c4efdf534ad841a86cd5e844a835e46e8e75d45738809",
        );
    if let Some(home) = dirs::home_dir() {
        cmd.env("HOME", home);
    }
    apply_coverage_env(&mut cmd);
    cmd.output().expect("failed to run tako")
}

fn wait_for_ssh_ready(ssh_port: u16) {
    let mut last_error = String::new();
    for _ in 0..120 {
        let out = Command::new("ssh")
            .args([
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "UserKnownHostsFile=/dev/null",
                "-o",
                "BatchMode=yes",
                "-p",
                &ssh_port.to_string(),
                "tako@127.0.0.1",
                "echo ok",
            ])
            .output()
            .expect("ssh readiness check failed");
        if out.status.success() && String::from_utf8_lossy(&out.stdout).contains("ok") {
            return;
        }
        last_error = String::from_utf8_lossy(&out.stderr).trim().to_string();
        thread::sleep(Duration::from_millis(250));
    }

    panic!(
        "ssh server did not become ready on port {}: {}",
        ssh_port, last_error
    );
}

fn write_deploy_artifacts(project: &Path) {
    let artifact_dir = project.join(".tako").join("artifacts").join("app");
    fs::create_dir_all(&artifact_dir).unwrap();
    fs::copy(project.join("tako.toml"), artifact_dir.join("tako.toml")).unwrap();
    fs::copy(
        project.join("package.json"),
        artifact_dir.join("package.json"),
    )
    .unwrap();
    fs::copy(project.join("index.ts"), artifact_dir.join("index.ts")).unwrap();
}

fn should_skip_fixture_entry(name: &str) -> bool {
    matches!(name, "node_modules" | ".tako" | ".output" | ".tanstack")
}

fn copy_directory_recursive(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();

    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if should_skip_fixture_entry(name.to_string_lossy().as_ref()) {
            continue;
        }
        let source_path = entry.path();
        let target_path = target.join(&name);
        let file_type = entry.file_type().unwrap();

        if file_type.is_dir() {
            copy_directory_recursive(&source_path, &target_path);
            continue;
        }

        if file_type.is_file() {
            fs::copy(&source_path, &target_path).unwrap();
            continue;
        }

        if file_type.is_symlink() {
            continue;
        }

        panic!(
            "unsupported fixture entry type at {}",
            source_path.display()
        );
    }
}

fn copy_directory_contents(source: &Path, target: &Path) {
    fs::create_dir_all(target).unwrap();
    for entry in fs::read_dir(source).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        if should_skip_fixture_entry(name.to_string_lossy().as_ref()) {
            continue;
        }
        let source_path = entry.path();
        let target_path = target.join(&name);
        let file_type = entry.file_type().unwrap();

        if file_type.is_dir() {
            copy_directory_recursive(&source_path, &target_path);
        } else if file_type.is_file() {
            fs::copy(&source_path, &target_path).unwrap();
        } else if file_type.is_symlink() {
            continue;
        } else {
            panic!(
                "unsupported fixture entry type at {}",
                source_path.display()
            );
        }
    }
}

fn disable_build_script(package_json_path: &Path) {
    let contents = fs::read_to_string(package_json_path).unwrap();
    let mut package_json: serde_json::Value = serde_json::from_str(&contents).unwrap();
    if let Some(scripts) = package_json
        .get_mut("scripts")
        .and_then(|v| v.as_object_mut())
    {
        scripts.remove("build");
    }
    let serialized = serde_json::to_string_pretty(&package_json).unwrap();
    fs::write(package_json_path, format!("{serialized}\n")).unwrap();
}

fn stage_fixture_artifacts(project: &Path) {
    let source = project.join("deploy-artifacts");
    assert!(
        source.is_dir(),
        "fixture must contain deploy-artifacts directory: {}",
        source.display()
    );

    let artifact_dir = project.join(".tako").join("artifacts").join("app");
    if artifact_dir.exists() {
        fs::remove_dir_all(&artifact_dir).unwrap();
    }
    fs::create_dir_all(&artifact_dir).unwrap();
    copy_directory_contents(&source, &artifact_dir);
}

#[test]
fn deploy_e2e_partial_failure_reports_failed_server() {
    if std::env::var("TAKO_E2E").is_err() {
        return;
    }
    if !docker_ok() {
        return;
    }

    let tag = "tako-deploy-e2e:latest";
    build_image(tag);

    let temp = TempDir::new().unwrap();
    let tako_home = temp.path().join("tako-home");
    fs::create_dir_all(&tako_home).unwrap();

    let Some(pubkey) = local_ssh_public_key() else {
        return;
    };

    let id = docker_stdout(&[
        "run",
        "-d",
        "-e",
        &format!("AUTHORIZED_KEY={}", pubkey.trim()),
        "-p",
        "127.0.0.1::22",
        tag,
    ]);
    let _c = DockerContainer { id: id.clone() };

    let port_line = docker_stdout(&["port", &id, "22/tcp"]);
    let ssh_port: u16 = port_line
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .expect("failed to parse docker port");
    wait_for_ssh_ready(ssh_port);

    // Prepare project.
    let project = temp.path().join("app");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("bun.lockb"), "").unwrap();
    fs::write(project.join("package.json"), r#"{"name":"test-app"}"#).unwrap();
    fs::write(project.join("index.ts"), "export default {}\n").unwrap();

    fs::write(
        project.join("tako.toml"),
        r#"
[app]
name = "test-app"

[envs.production]
routes = ["test-app.example.com"]

[servers.good]
env = "production"

[servers.bad]
env = "production"
"#,
    )
    .unwrap();
    write_deploy_artifacts(&project);

    // Global server inventory lives at ${TAKO_HOME}/config.toml
    // Use a second server that fails fast via connection refused.
    fs::write(
        tako_home.join("config.toml"),
        format!(
            "[[servers]]\nname = \"good\"\nhost = \"127.0.0.1\"\nport = {}\n\n[[servers]]\nname = \"bad\"\nhost = \"localhost\"\nport = 1\n\n[server_targets.good]\narch = \"x86_64\"\nlibc = \"musl\"\n\n[server_targets.bad]\narch = \"x86_64\"\nlibc = \"musl\"\n",
            ssh_port
        ),
    )
    .unwrap();

    let out = run_tako(&["deploy", "--env", "production"], &project, &tako_home);
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        !out.status.success(),
        "expected deploy to fail: {}",
        combined
    );
    assert!(
        combined.contains("Deployment partially failed") || combined.contains("server(s) failed"),
        "unexpected output: {}",
        combined
    );
    assert!(
        combined.contains("bad"),
        "expected bad server error: {}",
        combined
    );
}

#[test]
fn deploy_e2e_docker_alpine() {
    if std::env::var("TAKO_E2E").is_err() {
        return;
    }
    if !docker_ok() {
        return;
    }

    let tag = "tako-deploy-e2e:latest";
    build_image(tag);

    let temp = TempDir::new().unwrap();
    let tako_home = temp.path().join("tako-home");
    fs::create_dir_all(&tako_home).unwrap();

    let Some(pubkey) = local_ssh_public_key() else {
        return;
    };

    let id = docker_stdout(&[
        "run",
        "-d",
        "-e",
        &format!("AUTHORIZED_KEY={}", pubkey.trim()),
        "-p",
        "127.0.0.1::22",
        tag,
    ]);
    let _c = DockerContainer { id: id.clone() };

    // Discover mapped SSH port.
    let port_line = docker_stdout(&["port", &id, "22/tcp"]);
    // Example: 127.0.0.1:49154
    let ssh_port: u16 = port_line
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .expect("failed to parse docker port");
    wait_for_ssh_ready(ssh_port);

    // Prepare project.
    let project = temp.path().join("app");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("bun.lockb"), "").unwrap();
    fs::write(project.join("package.json"), r#"{"name":"test-app"}"#).unwrap();
    fs::write(project.join("index.ts"), "export default {}\n").unwrap();

    fs::write(
        project.join("tako.toml"),
        r#"
[app]
name = "test-app"

[envs.production]
routes = ["test-app.example.com"]

[servers.ssh]
env = "production"
"#,
    )
    .unwrap();
    write_deploy_artifacts(&project);

    // Global server inventory lives at ${TAKO_HOME}/config.toml
    fs::write(
        tako_home.join("config.toml"),
        format!(
            "[[servers]]\nname = \"ssh\"\nhost = \"127.0.0.1\"\nport = {}\n\n[server_targets.ssh]\narch = \"x86_64\"\nlibc = \"musl\"\n",
            ssh_port
        ),
    )
    .unwrap();

    // Deploy.
    let out = run_tako(&["deploy", "--env", "production"], &project, &tako_home);
    assert!(
        out.status.success(),
        "deploy failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let version = extract_version(&combined).expect("failed to parse deploy version");

    // Verify remote side-effect: last_deploy.json written.
    let ssh = |cmd: &str| {
        Command::new("ssh")
            .args([
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "BatchMode=yes",
                "-p",
                &ssh_port.to_string(),
                "tako@127.0.0.1",
                cmd,
            ])
            .output()
            .expect("ssh failed")
    };

    let out = ssh("test -f /opt/tako/last_deploy.json && echo ok");
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("ok"));

    // Verify remote release layout created by the CLI.
    let release_dir = format!("/opt/tako/apps/test-app/releases/{}", version);

    let out = ssh(&format!("test -d {} && echo ok", release_dir));
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("ok"));

    // Check expected files extracted into release.
    let out = ssh(&format!(
        "test -f {0}/tako.toml && test -f {0}/index.ts && test -f {0}/package.json && echo ok",
        release_dir
    ));
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("ok"));

    // current symlink points at the release.
    let out =
        ssh("test -L /opt/tako/apps/test-app/current && readlink /opt/tako/apps/test-app/current");
    assert!(out.status.success());
    let current_target = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(current_target, release_dir);

    // .env contains the build version.
    let out = ssh(&format!(
        "grep -F 'TAKO_BUILD=\"{}\"' {}/.env && echo ok",
        version, release_dir
    ));
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("ok"));

    // logs is a symlink into shared/logs.
    let out = ssh(&format!(
        "test -L {}/logs && readlink {}/logs",
        release_dir, release_dir
    ));
    assert!(out.status.success());
    let logs_target = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(
        logs_target,
        "/opt/tako/apps/test-app/shared/logs".to_string()
    );
}

#[test]
fn deploy_e2e_respects_remote_lock() {
    if std::env::var("TAKO_E2E").is_err() {
        return;
    }
    if !docker_ok() {
        return;
    }

    let tag = "tako-deploy-e2e:latest";
    build_image(tag);

    let temp = TempDir::new().unwrap();
    let tako_home = temp.path().join("tako-home");
    fs::create_dir_all(&tako_home).unwrap();

    let Some(pubkey) = local_ssh_public_key() else {
        return;
    };

    let id = docker_stdout(&[
        "run",
        "-d",
        "-e",
        &format!("AUTHORIZED_KEY={}", pubkey.trim()),
        "-p",
        "127.0.0.1::22",
        tag,
    ]);
    let _c = DockerContainer { id: id.clone() };

    let port_line = docker_stdout(&["port", &id, "22/tcp"]);
    let ssh_port: u16 = port_line
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .expect("failed to parse docker port");
    wait_for_ssh_ready(ssh_port);

    let project = temp.path().join("app");
    fs::create_dir_all(&project).unwrap();
    fs::write(project.join("bun.lockb"), "").unwrap();
    fs::write(project.join("package.json"), r#"{"name":"test-app"}"#).unwrap();
    fs::write(project.join("index.ts"), "export default {}\n").unwrap();
    fs::write(
        project.join("tako.toml"),
        r#"
[app]
name = "test-app"

[envs.production]
routes = ["test-app.example.com"]

[servers.ssh]
env = "production"
"#,
    )
    .unwrap();
    write_deploy_artifacts(&project);

    fs::write(
        tako_home.join("config.toml"),
        format!(
            "[[servers]]\nname = \"ssh\"\nhost = \"127.0.0.1\"\nport = {}\n\n[server_targets.ssh]\narch = \"x86_64\"\nlibc = \"musl\"\n",
            ssh_port
        ),
    )
    .unwrap();

    let ssh = |cmd: &str| {
        Command::new("ssh")
            .args([
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "BatchMode=yes",
                "-p",
                &ssh_port.to_string(),
                "tako@127.0.0.1",
                cmd,
            ])
            .output()
            .expect("ssh failed")
    };

    // Pre-create the lock dir.
    let out = ssh(
        "mkdir -p /opt/tako/apps/test-app && mkdir /opt/tako/apps/test-app/.deploy_lock && echo locked",
    );
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("locked"));

    let out = run_tako(&["deploy", "--env", "production"], &project, &tako_home);
    assert!(!out.status.success(), "deploy should fail when locked");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(combined.to_lowercase().contains("lock"));
}

#[test]
fn deploy_e2e_workspace_fixture_docker_alpine() {
    if std::env::var("TAKO_E2E").is_err() {
        return;
    }
    if !docker_ok() {
        return;
    }

    let fixture_rel =
        std::env::var("TAKO_E2E_APP").unwrap_or_else(|_| "e2e/js/tanstack-start".to_string());
    let fixture_dir = workspace_root().join(&fixture_rel);
    assert!(
        fixture_dir.is_dir(),
        "fixture directory not found: {}",
        fixture_dir.display()
    );

    let tag = "tako-deploy-e2e:latest";
    build_image(tag);

    let temp = TempDir::new().unwrap();
    let tako_home = temp.path().join("tako-home");
    fs::create_dir_all(&tako_home).unwrap();

    let Some(pubkey) = local_ssh_public_key() else {
        return;
    };

    let id = docker_stdout(&[
        "run",
        "-d",
        "-e",
        &format!("AUTHORIZED_KEY={}", pubkey.trim()),
        "-p",
        "127.0.0.1::22",
        tag,
    ]);
    let _c = DockerContainer { id: id.clone() };

    let port_line = docker_stdout(&["port", &id, "22/tcp"]);
    let ssh_port: u16 = port_line
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .expect("failed to parse docker port");
    wait_for_ssh_ready(ssh_port);

    let project = temp.path().join("app");
    copy_directory_recursive(&fixture_dir, &project);
    disable_build_script(&project.join("package.json"));
    stage_fixture_artifacts(&project);

    fs::write(
        tako_home.join("config.toml"),
        format!(
            "[[servers]]\nname = \"ssh\"\nhost = \"127.0.0.1\"\nport = {}\n\n[server_targets.ssh]\narch = \"x86_64\"\nlibc = \"musl\"\n",
            ssh_port
        ),
    )
    .unwrap();

    let out = run_tako(&["deploy", "--env", "production"], &project, &tako_home);
    assert!(
        out.status.success(),
        "deploy failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );

    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let version = extract_version(&combined).expect("failed to parse deploy version");

    let ssh = |cmd: &str| {
        Command::new("ssh")
            .args([
                "-o",
                "StrictHostKeyChecking=no",
                "-o",
                "BatchMode=yes",
                "-p",
                &ssh_port.to_string(),
                "tako@127.0.0.1",
                cmd,
            ])
            .output()
            .expect("ssh failed")
    };

    let release_dir = format!("/opt/tako/apps/tanstack-start-e2e/releases/{}", version);
    let out = ssh(&format!("test -d {} && echo ok", release_dir));
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("ok"));

    let out = ssh(&format!(
        "test -f {0}/package.json && test -f {0}/index.ts && test -f {0}/server/index.mjs && test -f {0}/static/index.html && test -f {0}/static/assets/main.js && echo ok",
        release_dir
    ));
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("ok"));

    let out = ssh(
        "test -L /opt/tako/apps/tanstack-start-e2e/current && readlink /opt/tako/apps/tanstack-start-e2e/current",
    );
    assert!(out.status.success());
    let current_target = String::from_utf8_lossy(&out.stdout).trim().to_string();
    assert_eq!(current_target, release_dir);
}
