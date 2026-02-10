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

    // First request should cold start and then succeed.
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
