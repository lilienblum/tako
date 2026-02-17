mod support;

use std::fs;
use std::time::Duration;

use support::{TestServer, bun_ok, can_bind_local_ports, wait_for, write_bun_app};

use tempfile::TempDir;

#[test]
fn on_demand_cold_start_and_idle_scale_to_zero() {
    if !bun_ok() {
        return;
    }
    if !can_bind_local_ports() {
        return;
    }

    let mut last_error = String::new();
    for attempt in 1..=3 {
        match run_on_demand_case() {
            Ok(()) => return,
            Err(err) => {
                last_error = err;
                if attempt < 3 {
                    eprintln!(
                        "on_demand_cold_start_and_idle_scale_to_zero retry {attempt}/3 failed: {}",
                        last_error
                    );
                }
            }
        }
    }

    panic!("on-demand flow did not stabilize after retries: {last_error}");
}

fn run_on_demand_case() -> Result<(), String> {
    let server = TestServer::start();
    let temp = TempDir::new().map_err(|e| e.to_string())?;
    let app_dir = temp.path().join("app");
    fs::create_dir_all(&app_dir).map_err(|e| e.to_string())?;
    write_bun_app(&app_dir, "hello");

    let host = "test.localhost";

    let resp = server.send_command(&serde_json::json!({
        "command": "deploy",
        "app": "test-app",
        "version": "v1",
        "path": app_dir.to_string_lossy(),
        "routes": [host],
        "instances": 0,
        "idle_timeout": 1,
    }));
    if resp.get("status").and_then(|s| s.as_str()) != Some("ok") {
        return Err(format!("deploy failed: {resp:?}"));
    }

    // Deploy should leave one warm instance so the app is immediately reachable.
    let warm_status = server.send_command(&serde_json::json!({
        "command": "status",
        "app": "test-app",
    }));
    let Some(warm_data) = warm_status.get("data") else {
        return Err(format!(
            "missing status payload after deploy: {warm_status:?}"
        ));
    };
    let Some(warm_instances) = warm_data.get("instances").and_then(|v| v.as_array()) else {
        return Err(format!(
            "missing instance list in status payload after deploy: {warm_status:?}"
        ));
    };
    if warm_instances.len() != 1 {
        return Err(format!(
            "expected one warm instance after on-demand deploy, got {}: {warm_status:?}",
            warm_instances.len()
        ));
    }

    // First request should succeed (warm instance may already be running).
    let mut first_last_status = String::new();
    let first_ok = wait_for(Duration::from_secs(30), || {
        match server.https_status(host, "/") {
            Ok(200) => true,
            Ok(code) => {
                first_last_status = format!("status {code}");
                false
            }
            Err(e) => {
                first_last_status = e;
                false
            }
        }
    });
    if !first_ok {
        return Err(format!(
            "first cold start request never returned HTTP 200; last response: {}",
            first_last_status
        ));
    }

    // Eventually idle monitor stops the instance and reports no instances.
    let mut status_snapshot = serde_json::Value::Null;
    let idle_ok = wait_for(Duration::from_secs(20), || {
        let resp = server.send_command(&serde_json::json!({
            "command": "status",
            "app": "test-app",
        }));
        status_snapshot = resp.clone();
        let Some(data) = resp.get("data") else {
            return false;
        };
        let Some(instances) = data.get("instances").and_then(|v| v.as_array()) else {
            return false;
        };
        instances.is_empty()
    });
    if !idle_ok {
        return Err(format!(
            "instance never scaled to zero after idle timeout; last status: {status_snapshot:?}"
        ));
    }

    // Request again should cold start again.
    let mut second_last_status = String::new();
    let second_ok = wait_for(Duration::from_secs(30), || {
        match server.https_status(host, "/") {
            Ok(200) => true,
            Ok(code) => {
                second_last_status = format!("status {code}");
                false
            }
            Err(e) => {
                second_last_status = e;
                false
            }
        }
    });
    if !second_ok {
        return Err(format!(
            "second cold start request never returned HTTP 200; last response: {}",
            second_last_status
        ));
    }

    Ok(())
}

#[test]
fn on_demand_startup_failure_does_not_hang() {
    if !bun_ok() {
        return;
    }
    if !can_bind_local_ports() {
        return;
    }

    let server = TestServer::start();
    let temp = TempDir::new().expect("create temp dir");
    let app_dir = temp.path().join("app");
    fs::create_dir_all(&app_dir).expect("create app dir");
    fs::create_dir_all(app_dir.join("src")).expect("create src dir");
    fs::write(
        app_dir.join("package.json"),
        r#"{"name":"test-app","scripts":{"dev":"bun src/index.ts"}}"#,
    )
    .expect("write package.json");
    fs::write(
        app_dir.join("src/index.ts"),
        r#"process.exit(1);
"#,
    )
    .expect("write failing app");

    let host = "failing.localhost";
    let resp = server.send_command(&serde_json::json!({
        "command": "deploy",
        "app": "failing-app",
        "version": "v1",
        "path": app_dir.to_string_lossy(),
        "routes": [host],
        "instances": 0,
        "idle_timeout": 1,
    }));
    match resp.get("status").and_then(|s| s.as_str()) {
        Some("error") => {
            let message = resp.get("message").and_then(|v| v.as_str()).unwrap_or("");
            assert!(
                message.contains("Warm instance startup failed")
                    || message.contains("Invalid app release"),
                "unexpected deploy failure message: {resp:?}"
            );
        }
        Some("ok") => match server.https_status(host, "/") {
            Ok(502) => {}
            Ok(code) => panic!("expected 502 for failing on-demand startup, got HTTP {code}"),
            Err(err) => panic!("expected completed 502 response, got request error: {err}"),
        },
        other => panic!("unexpected deploy response status {other:?}: {resp:?}"),
    }
}
