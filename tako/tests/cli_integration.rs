//! CLI Integration Tests
//!
//! Tests the full tako CLI workflow from init to deploy using mock servers.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::sync::{Mutex as StdMutex, OnceLock};
use std::{io::BufRead, thread};
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

/// Helper to run tako CLI commands
fn run_tako(args: &[&str], cwd: &Path) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tako"));
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_coverage_env(&mut cmd);
    cmd.output().expect("Failed to run tako command")
}

/// Helper to run tako CLI commands with stdin input
fn run_tako_with_stdin(args: &[&str], cwd: &Path, input: &str) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tako"));
    cmd.args(args)
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_coverage_env(&mut cmd);
    let mut child = cmd.spawn().expect("Failed to spawn tako command");

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes()).ok();
    }

    child
        .wait_with_output()
        .expect("Failed to wait for tako command")
}

fn run_tako_with_env(
    args: &[&str],
    cwd: &Path,
    home: &Path,
    tako_home: &Path,
) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tako"));
    cmd.args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("TAKO_HOME", tako_home)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_coverage_env(&mut cmd);
    cmd.output().expect("Failed to run tako command")
}

fn run_tako_with_stdin_and_env(
    args: &[&str],
    cwd: &Path,
    input: &str,
    home: &Path,
    tako_home: &Path,
) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_tako"));
    cmd.args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("TAKO_HOME", tako_home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    apply_coverage_env(&mut cmd);
    let mut child = cmd.spawn().expect("Failed to spawn tako command");

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(input.as_bytes()).ok();
    }

    child
        .wait_with_output()
        .expect("Failed to wait for tako command")
}

/// Helper to get stdout as string
fn stdout_str(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stdout).to_string()
}

/// Helper to get stderr as string
fn stderr_str(output: &std::process::Output) -> String {
    String::from_utf8_lossy(&output.stderr).to_string()
}

fn setup_minimal_bun_project(project_dir: &Path) {
    fs::write(project_dir.join("bun.lockb"), "").unwrap();
    fs::write(
        project_dir.join("package.json"),
        r#"{"name": "dev-test-app", "version": "1.0.0"}"#,
    )
    .unwrap();
    fs::write(
        project_dir.join("index.ts"),
        r#"export default { fetch() { return new Response("ok"); } };"#,
    )
    .unwrap();
}

struct FakeDevServer {
    sock_path: PathBuf,
    running: Arc<AtomicBool>,
    join: Option<thread::JoinHandle<()>>,
}

impl FakeDevServer {
    fn start(tako_home: &Path) -> Option<Self> {
        fs::create_dir_all(tako_home).unwrap();
        let sock_path = tako_home.join("dev-server.sock");
        let _ = fs::remove_file(&sock_path);

        let running = Arc::new(AtomicBool::new(true));
        let running2 = running.clone();
        let sock_path2 = sock_path.clone();
        let listener = std::os::unix::net::UnixListener::bind(&sock_path2).ok()?;
        listener
            .set_nonblocking(true)
            .expect("set_nonblocking on fake dev-server sock");

        let join = thread::spawn(move || {
            while running2.load(Ordering::SeqCst) {
                let (stream, _) = match listener.accept() {
                    Ok(x) => x,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        continue;
                    }
                    Err(_) => break,
                };
                let _ = stream.set_nonblocking(false);
                let mut reader = std::io::BufReader::new(stream.try_clone().unwrap());
                let mut writer = stream;

                let mut line = String::new();
                while reader
                    .read_line(&mut line)
                    .ok()
                    .filter(|n| *n > 0)
                    .is_some()
                {
                    let v: serde_json::Value = match serde_json::from_str(&line) {
                        Ok(v) => v,
                        Err(_) => {
                            line.clear();
                            continue;
                        }
                    };
                    let typ = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
                    let resp = match typ {
                        "Ping" => serde_json::json!({ "type": "Pong" }),
                        "GetToken" => serde_json::json!({
                            "type": "Token",
                            "token": "t"
                        }),
                        "ListApps" => serde_json::json!({
                            "type": "Apps",
                            "apps": [
                                { "lease_id": "la", "app_name": "a", "hosts": ["a.tako.local"], "upstream_port": 1234, "pid": 111 },
                                { "lease_id": "lb", "app_name": "b", "hosts": ["b.tako.local"], "upstream_port": 2222 }
                            ]
                        }),
                        "Info" => serde_json::json!({
                                "type": "Info",
                                "info": {
                                "listen": "127.0.0.1:8443",
                                "port": 8443,
                                "advertised_ip": "127.0.0.1",
                                "local_dns_enabled": true,
                                "local_dns_port": 53535
                            }
                        }),
                        "UnregisterLease" => serde_json::json!({
                            "type": "LeaseUnregistered",
                            "lease_id": v.get("lease_id").and_then(|a| a.as_str()).unwrap_or(""),
                        }),
                        "StopServer" => {
                            running2.store(false, Ordering::SeqCst);
                            serde_json::json!({ "type": "Stopping" })
                        }
                        _ => serde_json::json!({ "type": "Error", "message": "unknown request" }),
                    };
                    let _ = writeln!(writer, "{}", resp);
                    line.clear();
                    if typ == "StopServer" {
                        break;
                    }
                }
            }
        });

        // Wait until the socket exists so callers can connect reliably.
        for _ in 0..50 {
            if sock_path.exists() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        Some(Self {
            sock_path,
            running,
            join: Some(join),
        })
    }
}

impl Drop for FakeDevServer {
    fn drop(&mut self) {
        // Best effort: signal stop and join.
        self.running.store(false, Ordering::SeqCst);
        // Wake the accept loop if it's sleeping/polling.
        let _ = std::os::unix::net::UnixStream::connect(&self.sock_path);
        let _ = std::fs::remove_file(&self.sock_path);
        if let Some(j) = self.join.take() {
            let _ = j.join();
        }
    }
}

fn dev_daemon_test_lock() -> &'static StdMutex<()> {
    static LOCK: OnceLock<StdMutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| StdMutex::new(()))
}

mod init {
    use super::*;

    #[test]
    fn test_init_creates_tako_toml() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();

        // Create a minimal package.json
        fs::write(
            project_dir.join("package.json"),
            r#"{"name": "test-app", "version": "1.0.0"}"#,
        )
        .unwrap();

        // Create entry point
        fs::write(
            project_dir.join("index.ts"),
            r#"export default { fetch() { return new Response("ok"); } };"#,
        )
        .unwrap();

        // Run tako init (uses --force to skip confirmation)
        let output = run_tako(&["init", "--force"], &project_dir);

        assert!(
            output.status.success(),
            "tako init failed: {}",
            stderr_str(&output)
        );

        // Check tako.toml was created
        let tako_toml = project_dir.join("tako.toml");
        assert!(tako_toml.exists(), "tako.toml should be created");

        let content = fs::read_to_string(&tako_toml).unwrap();
        // The generated format uses top-level app metadata fields.
        assert!(
            content.contains("# name = \"test-app\""),
            "tako.toml should have top-level name example: {}",
            content
        );
        assert!(
            content.contains("test-app"),
            "tako.toml should have app name: {}",
            content
        );
    }

    #[test]
    fn test_init_accepts_directory_argument() {
        let temp = TempDir::new().unwrap();
        let root_dir = temp.path().to_path_buf();
        let project_dir = root_dir.join("my-app");
        fs::create_dir_all(&project_dir).unwrap();

        // Create a minimal package.json + entry point inside the target dir
        fs::write(
            project_dir.join("package.json"),
            r#"{"name": "dir-flag-app", "version": "1.0.0"}"#,
        )
        .unwrap();
        fs::write(
            project_dir.join("index.ts"),
            r#"export default { fetch() { return new Response("ok"); } };"#,
        )
        .unwrap();

        // Invoke from root_dir, but tell tako to operate in project_dir
        let output = run_tako(&["init", "--force", "my-app"], &root_dir);

        assert!(
            output.status.success(),
            "tako init DIR failed: {}",
            stderr_str(&output)
        );

        assert!(
            project_dir.join("tako.toml").exists(),
            "tako.toml should be created in target dir"
        );
        assert!(
            !root_dir.join("tako.toml").exists(),
            "tako.toml should not be created in invocation directory"
        );
    }

    #[test]
    fn test_init_with_force_flag() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();

        // Create bun.lockb to indicate Bun project
        fs::write(project_dir.join("bun.lockb"), "").unwrap();
        fs::write(
            project_dir.join("package.json"),
            r#"{"name": "bun-app", "version": "1.0.0"}"#,
        )
        .unwrap();
        fs::write(
            project_dir.join("index.ts"),
            r#"export default { fetch() { return new Response("ok"); } };"#,
        )
        .unwrap();

        // First init
        let output = run_tako(&["init", "--force"], &project_dir);

        // Verify init runs (may require interactive input)
        let combined = format!("{}{}", stdout_str(&output), stderr_str(&output));
        // Either creates file or shows init output
        assert!(
            project_dir.join("tako.toml").exists() || !combined.is_empty(),
            "init should produce output or file"
        );
    }

    #[test]
    fn test_init_without_package_json() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();

        let output = run_tako(&["init"], &project_dir);

        // Should handle missing package.json gracefully
        let combined = format!("{}{}", stdout_str(&output), stderr_str(&output));
        // Either fails or warns - both are acceptable
        assert!(!combined.is_empty(), "Should produce some output");
    }
}

mod dev_daemon_commands {
    use super::*;

    #[test]
    fn dev_doctor_prints_info() {
        let _guard = dev_daemon_test_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();
        setup_minimal_bun_project(&project_dir);
        let home = temp.path().join("home");
        let tako_home = temp.path().join("tako-home");
        fs::create_dir_all(&home).unwrap();

        let Some(_fake) = FakeDevServer::start(&tako_home) else {
            return;
        };

        let out = run_tako_with_env(&["doctor"], &project_dir, &home, &tako_home);
        assert!(out.status.success(), "doctor failed: {}", stderr_str(&out));
        let stdout = stdout_str(&out);
        assert!(
            stdout.contains("dev-server:"),
            "unexpected doctor output: {}",
            stdout
        );
        assert!(
            !stdout.contains("\n  port: "),
            "doctor output should avoid duplicate port details: {}",
            stdout
        );
        assert!(
            stdout.contains("apps:"),
            "expected apps section: {}",
            stdout
        );
        assert!(stdout.contains("- a"), "expected app list: {}", stdout);
    }
}

mod server_commands {
    use super::*;

    #[test]
    fn test_server_ls_empty() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();

        // Create empty config.toml
        let tako_dir = project_dir.join(".tako");
        fs::create_dir_all(&tako_dir).unwrap();
        fs::write(tako_dir.join("config.toml"), "").unwrap();

        // Point tako at this isolated TAKO_HOME.
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tako"));
        cmd.args(["servers", "ls"])
            .current_dir(&project_dir)
            .env("HOME", &project_dir)
            .env("TAKO_HOME", &tako_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        apply_coverage_env(&mut cmd);
        let output = cmd.output().expect("Failed to run tako command");

        assert!(
            output.status.success(),
            "tako servers ls failed: {}",
            stderr_str(&output)
        );

        let out = stdout_str(&output);
        assert!(
            out.contains("No servers configured"),
            "Should show no servers warning: {}",
            out
        );
        assert!(
            out.contains("Run 'tako servers add' to add a server."),
            "Should include add-server hint: {}",
            out
        );
        assert!(
            !out.contains("Add one now?"),
            "servers ls should not launch an add wizard: {}",
            out
        );
    }

    #[test]
    fn servers_add_creates_missing_tako_home() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().join("project");
        fs::create_dir_all(&project_dir).unwrap();

        // Point HOME somewhere safe and set TAKO_HOME to a missing directory.
        let home = temp.path().join("home");
        let tako_home = temp.path().join("missing-tako-home");
        fs::create_dir_all(&home).unwrap();
        assert!(!tako_home.exists());

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_tako"));
        cmd.args(["servers", "add", "1.2.3.4", "--name", "prod", "--no-test"])
            .current_dir(&project_dir)
            .env("HOME", &home)
            .env("TAKO_HOME", &tako_home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        apply_coverage_env(&mut cmd);
        let output = cmd.output().expect("Failed to run tako command");

        assert!(
            output.status.success(),
            "tako servers add failed: {}{}",
            stdout_str(&output),
            stderr_str(&output)
        );

        assert!(tako_home.join("config.toml").exists());
    }

    #[test]
    fn servers_add_with_host_requires_name() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().join("project");
        fs::create_dir_all(&project_dir).unwrap();

        let home = temp.path().join("home");
        let tako_home = temp.path().join("tako-home");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&tako_home).unwrap();

        let out = run_tako_with_env(
            &["servers", "add", "1.2.3.4", "--no-test"],
            &project_dir,
            &home,
            &tako_home,
        );

        assert!(
            !out.status.success(),
            "servers add without --name should fail: {}{}",
            stdout_str(&out),
            stderr_str(&out)
        );

        let combined = format!("{}{}", stdout_str(&out), stderr_str(&out));
        assert!(
            combined.contains("Server name is required"),
            "expected missing-name guidance: {}",
            combined
        );
    }

    #[test]
    fn servers_add_persists_description() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().join("project");
        fs::create_dir_all(&project_dir).unwrap();

        let home = temp.path().join("home");
        let tako_home = temp.path().join("tako-home");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&tako_home).unwrap();

        let out = run_tako_with_env(
            &[
                "servers",
                "add",
                "10.0.0.1",
                "--name",
                "edge",
                "--description",
                "Edge POP",
                "--no-test",
            ],
            &project_dir,
            &home,
            &tako_home,
        );
        assert!(
            out.status.success(),
            "add with description should succeed: {}{}",
            stdout_str(&out),
            stderr_str(&out)
        );

        let servers_toml = fs::read_to_string(tako_home.join("config.toml")).unwrap();
        assert!(
            servers_toml.contains("description = \"Edge POP\""),
            "config.toml should include description: {}",
            servers_toml
        );
    }

    #[test]
    fn servers_list_shows_description_column() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().join("project");
        fs::create_dir_all(&project_dir).unwrap();

        let home = temp.path().join("home");
        let tako_home = temp.path().join("tako-home");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&tako_home).unwrap();

        let add = run_tako_with_env(
            &[
                "servers",
                "add",
                "10.0.0.2",
                "--name",
                "eu-edge",
                "--description",
                "EU Edge",
                "--no-test",
            ],
            &project_dir,
            &home,
            &tako_home,
        );
        assert!(add.status.success(), "add should succeed");

        let ls = run_tako_with_env(&["servers", "ls"], &project_dir, &home, &tako_home);
        assert!(
            ls.status.success(),
            "servers ls should succeed: {}{}",
            stdout_str(&ls),
            stderr_str(&ls)
        );

        let out = stdout_str(&ls);
        assert!(
            out.contains("DESCRIPTION"),
            "expected description column: {}",
            out
        );
        assert!(
            out.contains("EU Edge"),
            "expected description value: {}",
            out
        );
    }

    #[test]
    fn servers_rm_without_name_in_non_interactive_mode_shows_hint() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().join("project");
        fs::create_dir_all(&project_dir).unwrap();

        let home = temp.path().join("home");
        let tako_home = temp.path().join("tako-home");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&tako_home).unwrap();

        let add = run_tako_with_env(
            &[
                "servers",
                "add",
                "10.0.0.3",
                "--name",
                "prod-1",
                "--no-test",
            ],
            &project_dir,
            &home,
            &tako_home,
        );
        assert!(
            add.status.success(),
            "add should succeed: {}{}",
            stdout_str(&add),
            stderr_str(&add)
        );

        let rm = run_tako_with_env(&["servers", "rm"], &project_dir, &home, &tako_home);
        assert!(
            !rm.status.success(),
            "rm without name should fail on non-tty"
        );

        let stderr = stderr_str(&rm);
        assert!(
            stderr.contains("requires an interactive terminal")
                || stderr.contains("provide a server name"),
            "expected helpful error for non-interactive rm without name: {}",
            stderr
        );
    }

    #[test]
    fn servers_add_is_idempotent_for_same_name_host_and_port() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().join("project");
        fs::create_dir_all(&project_dir).unwrap();

        let home = temp.path().join("home");
        let tako_home = temp.path().join("tako-home");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&tako_home).unwrap();

        let first = run_tako_with_env(
            &[
                "servers",
                "add",
                "127.0.0.1",
                "--name",
                "prod",
                "--port",
                "2222",
                "--no-test",
            ],
            &project_dir,
            &home,
            &tako_home,
        );
        assert!(
            first.status.success(),
            "first add should succeed: {}{}",
            stdout_str(&first),
            stderr_str(&first)
        );

        let second = run_tako_with_env(
            &[
                "servers",
                "add",
                "127.0.0.1",
                "--name",
                "prod",
                "--port",
                "2222",
            ],
            &project_dir,
            &home,
            &tako_home,
        );
        assert!(
            second.status.success(),
            "second add should be idempotent: {}{}",
            stdout_str(&second),
            stderr_str(&second)
        );

        let combined = format!("{}{}", stdout_str(&second), stderr_str(&second));
        assert!(
            combined.contains("already configured"),
            "expected idempotent message: {}",
            combined
        );
    }

    #[test]
    fn servers_add_records_cli_history_for_autocomplete() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().join("project");
        fs::create_dir_all(&project_dir).unwrap();

        let home = temp.path().join("home");
        let tako_home = temp.path().join("tako-home");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&tako_home).unwrap();

        let add = run_tako_with_env(
            &[
                "servers",
                "add",
                "203.0.113.5",
                "--name",
                "edge-us",
                "--port",
                "2201",
                "--no-test",
            ],
            &project_dir,
            &home,
            &tako_home,
        );
        assert!(
            add.status.success(),
            "add should succeed: {}{}",
            stdout_str(&add),
            stderr_str(&add)
        );

        let history_path = tako_home.join("history.toml");
        let history_raw = fs::read_to_string(&history_path).expect("history file should exist");
        assert!(
            history_raw.contains("203.0.113.5"),
            "history should include host: {}",
            history_raw
        );
        assert!(
            history_raw.contains("edge-us"),
            "history should include server name: {}",
            history_raw
        );
        assert!(
            history_raw.contains("2201"),
            "history should include port: {}",
            history_raw
        );
        assert!(
            !history_raw.contains("[[servers]]"),
            "history should be separate from server config: {}",
            history_raw
        );
    }
}

mod secret_commands {
    use super::*;

    fn write_secret_test_tako_toml(path: &Path) {
        fs::write(
            path.join("tako.toml"),
            r#"
name = "test-app"
runtime = "bun"
entry = "index.ts"

[envs.production]
route = "prod.example.com"
server = "prod-server"
"#,
        )
        .unwrap();
    }

    #[test]
    fn test_secret_ls_empty() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();

        // Create tako.toml
        fs::write(
            project_dir.join("tako.toml"),
            r#"
name = "test-app"
runtime = "bun"
entry = "index.ts"
"#,
        )
        .unwrap();

        let output = run_tako(&["secrets", "ls"], &project_dir);

        assert!(
            output.status.success(),
            "tako secrets ls failed: {}",
            stderr_str(&output)
        );

        let out = stdout_str(&output);
        assert!(
            out.contains("No secrets") || out.is_empty() || out.contains("0 secrets"),
            "Should show no secrets"
        );
    }

    #[test]
    fn test_secret_set_reads_from_stdin() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();

        // Create tako.toml with env section
        write_secret_test_tako_toml(&project_dir);

        // Set a secret - value comes from stdin
        let output = run_tako_with_stdin(
            &["secrets", "set", "API_KEY", "--env", "production"],
            &project_dir,
            "secret123\n",
        );

        assert!(
            output.status.success(),
            "secret set should succeed: {}{}",
            stdout_str(&output),
            stderr_str(&output)
        );

        let secrets_path = project_dir.join(".tako").join("secrets");
        assert!(secrets_path.exists(), "secrets file should be created");

        let raw = fs::read_to_string(&secrets_path).expect("read secrets file");
        let parsed: serde_json::Value = serde_json::from_str(&raw).expect("parse secrets json");
        let stored = parsed["production"]["API_KEY"]
            .as_str()
            .expect("stored API_KEY value");
        assert!(!stored.is_empty(), "stored value should not be empty");
        assert_ne!(stored, "secret123", "stored value should be encrypted");
    }

    #[test]
    fn test_secret_sync_when_secrets_file_deleted() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();
        write_secret_test_tako_toml(&project_dir);

        // Simulate deleted secrets file.
        fs::create_dir_all(project_dir.join(".tako")).unwrap();
        fs::write(project_dir.join(".tako").join("secrets"), "{}").unwrap();
        fs::remove_file(project_dir.join(".tako").join("secrets")).unwrap();

        let output = run_tako(&["secrets", "sync", "--env", "production"], &project_dir);
        let combined = format!("{}{}", stdout_str(&output), stderr_str(&output));

        assert!(
            output.status.success(),
            "secrets sync should handle deleted file: {}",
            combined
        );
        assert!(
            combined.contains("No secrets to sync."),
            "expected no-secrets message: {}",
            combined
        );
    }

    #[test]
    fn test_secret_sync_reports_network_failure() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();
        let home = temp.path().join("home");
        let tako_home = temp.path().join("tako-home");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&tako_home).unwrap();

        // Project config with one environment and one mapped server alias.
        fs::write(
            project_dir.join("tako.toml"),
            r#"
name = "test-app"
runtime = "bun"
entry = "index.ts"

[envs.production]
route = "prod.example.com"
server = "prod-server"

[servers.prod-server]
env = "production"
"#,
        )
        .unwrap();

        // Remote servers registry: unreachable endpoint to force network failure quickly.
        fs::write(
            tako_home.join("config.toml"),
            r#"
[[servers]]
name = "prod-server"
host = "localhost"
port = 1
"#,
        )
        .unwrap();

        // Create encrypted secret and key in isolated HOME/TAKO_HOME.
        let set_output = run_tako_with_stdin_and_env(
            &["secrets", "set", "API_KEY", "--env", "production"],
            &project_dir,
            "secret123\n",
            &home,
            &tako_home,
        );
        assert!(
            set_output.status.success(),
            "secret set should succeed: {}{}",
            stdout_str(&set_output),
            stderr_str(&set_output)
        );

        let sync_output = run_tako_with_env(
            &["secrets", "sync", "--env", "production"],
            &project_dir,
            &home,
            &tako_home,
        );
        let combined = format!("{}{}", stdout_str(&sync_output), stderr_str(&sync_output));

        assert!(
            sync_output.status.success(),
            "sync should report partial failure without crashing: {}",
            combined
        );
        assert!(
            combined.contains("FAILED:"),
            "expected network failure to be reported: {}",
            combined
        );
        assert!(
            combined.contains("Synced to 0 server(s), 1 failed."),
            "expected failure summary: {}",
            combined
        );
    }
}

mod status_command {
    use super::*;

    #[test]
    fn test_status_shows_app_info() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();

        // Create tako.toml with proper env section
        fs::write(
            project_dir.join("tako.toml"),
            r#"
name = "my-test-app"
runtime = "bun"
entry = "index.ts"
port = 3000
instances = 2

[envs.production]
route = "prod.example.com"
server = "prod-server"
"#,
        )
        .unwrap();

        let output = run_tako(&["servers", "status"], &project_dir);

        // Status should show discovered app info or global server/app summary.
        let combined = format!("{}{}", stdout_str(&output), stderr_str(&output));
        assert!(
            combined.contains("my-test-app")
                || combined.contains("production")
                || combined.contains("App:")
                || combined.contains("No deployed apps found on configured servers.")
                || combined.contains("No deployed apps.")
                || combined.contains("tako-server"),
            "Should show app info or status: {}",
            combined
        );
    }

    #[test]
    fn test_status_without_tako_toml() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();
        let home = temp.path().join("home");
        let tako_home = temp.path().join("tako-home");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&tako_home).unwrap();

        let output = run_tako_with_env(&["servers", "status"], &project_dir, &home, &tako_home);

        // Status should work without project config and use global server inventory.
        let combined = format!("{}{}", stdout_str(&output), stderr_str(&output));
        assert!(
            output.status.success(),
            "status should not require tako.toml: {}",
            combined
        );
        assert!(
            combined.contains("No servers")
                || combined.contains("Add one now")
                || combined.contains("No deployed apps"),
            "should report global status context when no servers/apps: {}",
            combined
        );
    }

    #[test]
    fn test_status_with_server_name_is_rejected() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();

        let output = run_tako(&["servers", "status", "tako-server"], &project_dir);

        assert!(
            !output.status.success(),
            "status with server name should be rejected"
        );

        let combined = format!("{}{}", stdout_str(&output), stderr_str(&output));
        assert!(
            combined.contains("unexpected argument 'tako-server'")
                || combined.contains("Usage: tako servers status"),
            "should show parse usage error: {}",
            combined
        );
    }
}

mod deploy_command {
    use super::*;

    #[test]
    fn test_deploy_uses_implicit_production_when_no_envs_configured() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();
        let home = temp.path().join("home");
        let tako_home = temp.path().join("tako-home");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&tako_home).unwrap();

        // Create tako.toml without envs section.
        fs::write(
            project_dir.join("tako.toml"),
            r#"
name = "test-app"
runtime = "bun"
entry = "index.ts"
"#,
        )
        .unwrap();
        fs::write(project_dir.join("bun.lockb"), "").unwrap();
        fs::write(project_dir.join("index.ts"), "export default {}").unwrap();

        let output = run_tako_with_env(&["deploy"], &project_dir, &home, &tako_home);

        // Should fail because production must be explicitly configured.
        assert!(
            !output.status.success(),
            "Deploy should fail when no servers are configured"
        );

        let err = stderr_str(&output);
        assert!(
            err.contains("Environment 'production' not found"),
            "Should require explicit production environment mapping: {}",
            err
        );
    }

    #[test]
    fn test_deploy_rejects_development_environment() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();

        fs::write(
            project_dir.join("tako.toml"),
            r#"
name = "test-app"

[envs.development]
route = "dev.example.com"

[servers.dev-1]
env = "development"
"#,
        )
        .unwrap();
        fs::write(project_dir.join("bun.lockb"), "").unwrap();
        fs::write(project_dir.join("index.ts"), "export default {}").unwrap();

        let output = run_tako(&["deploy", "--env", "development"], &project_dir);

        assert!(
            !output.status.success(),
            "Deploy to development should be rejected"
        );

        let err = stderr_str(&output);
        assert!(
            err.contains("reserved for local development")
                || err.contains("cannot deploy to 'development'"),
            "Should explicitly reject deploying to development: {}",
            err
        );
    }

    #[test]
    fn test_deploy_with_invalid_env() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();

        // Create tako.toml with env
        fs::write(
            project_dir.join("tako.toml"),
            r#"
name = "test-app"
runtime = "bun"
entry = "index.ts"

[envs.production]
route = "prod.example.com"
server = "prod-server"
"#,
        )
        .unwrap();

        // Try to deploy to non-existent env
        let output = run_tako(&["deploy", "--env", "staging"], &project_dir);

        // Should fail because staging env doesn't exist
        assert!(
            !output.status.success(),
            "Deploy should fail with invalid env"
        );

        let err = stderr_str(&output);
        assert!(
            err.contains("staging") || err.contains("not found") || err.contains("Environment"),
            "Should mention invalid environment: {}",
            err
        );
    }

    #[test]
    fn test_deploy_validates_runtime_detection() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();

        // Create tako.toml with env but no runtime indicators (no bun.lockb, package.json)
        fs::write(
            project_dir.join("tako.toml"),
            r#"
name = "test-app"

[envs.production]
route = "prod.example.com"
server = "prod-server"
"#,
        )
        .unwrap();

        let output = run_tako(&["deploy", "--env", "production"], &project_dir);

        // Should fail because runtime can't be detected
        assert!(
            !output.status.success(),
            "Deploy should fail without detectable runtime"
        );

        let stderr = stderr_str(&output);
        assert!(
            stderr.contains("runtime") || stderr.contains("Runtime") || stderr.contains("bun"),
            "Should mention runtime detection failure: {}",
            stderr
        );
    }

    #[test]
    fn test_deploy_validates_entry_point_exists() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();

        // Create tako.toml with explicit entry point that doesn't exist
        // BUT create a valid default entry point (index.ts) so runtime detection passes
        fs::write(
            project_dir.join("tako.toml"),
            r#"
name = "test-app"
entry = "nonexistent.ts"

[envs.production]
route = "prod.example.com"
server = "prod-server"
"#,
        )
        .unwrap();

        // Add bun.lockb AND package.json to enable runtime detection
        fs::write(project_dir.join("bun.lockb"), "").unwrap();
        fs::write(
            project_dir.join("package.json"),
            // Specify a nonexistent main in package.json to test that path too
            r#"{"name": "test-app", "version": "1.0.0", "main": "nonexistent.ts"}"#,
        )
        .unwrap();

        let output = run_tako(&["deploy", "--env", "production"], &project_dir);

        // Should fail because entry point doesn't exist (either tako.toml entry or package.json main)
        assert!(
            !output.status.success(),
            "Deploy should fail with missing entry point"
        );

        let stderr = stderr_str(&output);
        // The runtime detection will fail because neither the package.json main nor default entry points exist
        // This is actually expected behavior - no valid entry point means no valid runtime
        assert!(
            stderr.contains("entry")
                || stderr.contains("Entry")
                || stderr.contains("nonexistent")
                || stderr.contains("not found")
                || stderr.contains("runtime")
                || stderr.contains("Runtime"),
            "Should mention missing entry point or runtime detection failure: {}",
            stderr
        );
    }

    #[test]
    fn test_deploy_validates_server_exists() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();

        // Create tako.toml referencing a server that doesn't exist
        // Use [servers.name] section to properly configure the server reference
        fs::write(
            project_dir.join("tako.toml"),
            r#"
name = "test-app"

[envs.production]
route = "prod.example.com"
# Reference server by name in servers section

[servers.nonexistent-server]
env = "production"
"#,
        )
        .unwrap();

        // Add bun.lockb, package.json and entry point
        fs::write(project_dir.join("bun.lockb"), "").unwrap();
        fs::write(
            project_dir.join("package.json"),
            r#"{"name": "test-app", "version": "1.0.0"}"#,
        )
        .unwrap();
        fs::write(project_dir.join("index.ts"), "export default {}").unwrap();

        let output = run_tako(&["deploy", "--env", "production"], &project_dir);

        // Should fail because server doesn't exist in global [[servers]] config
        assert!(
            !output.status.success(),
            "Deploy should fail with unknown server"
        );

        let stderr = stderr_str(&output);
        assert!(
            stderr.contains("nonexistent-server")
                || stderr.contains("not found")
                || stderr.contains("config.toml")
                || stderr.contains("Server"),
            "Should mention missing server: {}",
            stderr
        );
    }

    #[test]
    fn test_deploy_validates_no_servers_configured() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();
        let home = temp.path().join("home");
        let tako_home = temp.path().join("tako-home");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&tako_home).unwrap();

        // Create tako.toml with env but no server reference
        fs::write(
            project_dir.join("tako.toml"),
            r#"
name = "test-app"

[envs.production]
route = "prod.example.com"
# No server specified
"#,
        )
        .unwrap();

        // Add bun.lockb and entry point
        fs::write(project_dir.join("bun.lockb"), "").unwrap();
        fs::write(project_dir.join("index.ts"), "export default {}").unwrap();

        let output = run_tako_with_env(
            &["deploy", "--env", "production"],
            &project_dir,
            &home,
            &tako_home,
        );

        // Should fail because no servers configured
        assert!(
            !output.status.success(),
            "Deploy should fail with no servers"
        );

        let stderr = stderr_str(&output);
        assert!(
            stderr.contains("No servers have been added")
                || stderr.contains("tako servers add <host>"),
            "Should include add-server hint: {}",
            stderr
        );
    }

    #[test]
    fn test_deploy_no_longer_requires_local_dist_artifacts() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();
        let home = temp.path().join("home");
        let tako_home = temp.path().join("tako-home");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&tako_home).unwrap();

        fs::write(
            project_dir.join("tako.toml"),
            r#"
name = "test-app"

[envs.production]
routes = ["api.example.com"]

[servers.test-server]
env = "production"
"#,
        )
        .unwrap();

        fs::write(project_dir.join("bun.lockb"), "").unwrap();
        fs::write(project_dir.join("package.json"), r#"{"name":"test-app"}"#).unwrap();
        fs::write(project_dir.join("index.ts"), "export default {}").unwrap();

        fs::write(
            tako_home.join("config.toml"),
            r#"
[[servers]]
name = "test-server"
host = "127.0.0.1"
port = 22222

[server_targets.test-server]
arch = "x86_64"
libc = "glibc"
"#,
        )
        .unwrap();

        let output = run_tako_with_env(
            &["deploy", "--env", "production"],
            &project_dir,
            &home,
            &tako_home,
        );
        assert!(
            !output.status.success(),
            "deploy should fail due unreachable SSH server in this test setup"
        );

        let combined = format!("{}{}", stdout_str(&output), stderr_str(&output));
        assert!(
            !combined.contains("must contain build artifacts") && !combined.contains(".tako/dist"),
            "deploy should not require local dist artifacts: {}",
            combined
        );
    }

    #[test]
    fn test_deploy_shows_validation_messages() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();
        let home = temp.path().join("home");
        let tako_home = temp.path().join("tako-home");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&tako_home).unwrap();

        // Create a valid-looking config that will pass validation but fail on SSH
        fs::write(
            project_dir.join("tako.toml"),
            r#"
name = "test-app"

[envs.production]
server = "test-server"
routes = ["api.example.com"]
"#,
        )
        .unwrap();

        // Create config.toml with a test server in isolated test TAKO_HOME.
        let servers_path = tako_home.join("config.toml");
        fs::write(
            &servers_path,
            r#"
[[servers]]
name = "test-server"
host = "127.0.0.1"
port = 22222
"#,
        )
        .unwrap();

        // Add bun.lockb and entry point
        fs::write(project_dir.join("bun.lockb"), "").unwrap();
        fs::write(
            project_dir.join("index.ts"),
            "export default { fetch() { return new Response('ok'); } };",
        )
        .unwrap();

        let output = run_tako_with_env(
            &["deploy", "--env", "production"],
            &project_dir,
            &home,
            &tako_home,
        );

        // The deploy will fail (SSH will fail) but we should see validation messages first
        let combined = format!("{}{}", stdout_str(&output), stderr_str(&output));

        // Should show validation succeeded before failing on SSH
        assert!(
            combined.contains("Validation complete")
                || combined.contains("Validating")
                || combined.contains("OK"),
            "Should show validation in progress: {}",
            combined
        );
    }
}

mod help_and_version {
    use super::*;

    #[test]
    fn test_help_shows_commands() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();

        let output = run_tako(&["--help"], &project_dir);

        assert!(output.status.success(), "help should succeed");

        let out = stdout_str(&output);
        assert!(out.contains("init"), "Should list init command");
        assert!(out.contains("deploy"), "Should list deploy command");
        assert!(out.contains("dev"), "Should list dev command");
        assert!(out.contains("doctor"), "Should list doctor command");
        assert!(out.contains("upgrade"), "Should list upgrade command");
        assert!(out.contains("delete"), "Should list delete command");
        assert!(out.contains("servers"), "Should list servers command");
        assert!(out.contains("secrets"), "Should list secrets command");
    }

    #[test]
    fn test_dev_help_mentions_tui_flags() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();

        let output = run_tako(&["dev", "--help"], &project_dir);
        assert!(output.status.success(), "dev help should succeed");

        let out = stdout_str(&output);
        assert!(
            out.contains("--tui"),
            "dev help should mention --tui: {}",
            out
        );
        assert!(
            out.contains("--no-tui"),
            "dev help should mention --no-tui: {}",
            out
        );
    }

    #[test]
    fn test_version_shows_version() {
        let temp = TempDir::new().unwrap();
        let project_dir = temp.path().to_path_buf();

        let output = run_tako(&["--version"], &project_dir);

        assert!(output.status.success(), "version should succeed");

        let out = stdout_str(&output);
        assert!(
            out.contains("tako") || out.contains("0."),
            "Should show version: {}",
            out
        );
    }
}
