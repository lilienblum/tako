mod support;

use std::fs;
use std::time::Duration;

use support::{TestServer, bun_ok, can_bind_local_ports, wait_for, write_bun_app};

use tempfile::TempDir;

#[test]
fn rolling_update_deploy_updates_version_and_serves_new_code() {
    if !bun_ok() {
        return;
    }
    if !can_bind_local_ports() {
        return;
    }

    let server = TestServer::start();
    let temp = TempDir::new().unwrap();
    let app_dir = temp.path().join("app");
    fs::create_dir_all(&app_dir).unwrap();

    write_bun_app(&app_dir, "v1");

    let host = "test.localhost";

    let resp = server.send_command(&serde_json::json!({
        "command": "deploy",
        "app": "test-app",
        "version": "v1",
        "path": app_dir.to_string_lossy(),
        "routes": [host],
        "instances": 1,
        "idle_timeout": 300,
    }));
    assert_eq!(
        resp.get("status").and_then(|s| s.as_str()),
        Some("ok"),
        "{:?}",
        resp
    );

    let mut last_v1_body = String::new();
    assert!(
        wait_for(Duration::from_secs(30), || {
            let body = server.https_get(host, "/");
            if body.contains("v1") {
                return true;
            }
            last_v1_body = body;
            false
        }),
        "timed out waiting for v1 response, last body: {}",
        last_v1_body
    );

    // Deploy v2 (rolling update).
    write_bun_app(&app_dir, "v2");

    let resp = server.send_command(&serde_json::json!({
        "command": "deploy",
        "app": "test-app",
        "version": "v2",
        "path": app_dir.to_string_lossy(),
        "routes": [host],
        "instances": 1,
        "idle_timeout": 300,
    }));
    assert_eq!(
        resp.get("status").and_then(|s| s.as_str()),
        Some("ok"),
        "{:?}",
        resp
    );

    let mut last_v2_body = String::new();
    assert!(
        wait_for(Duration::from_secs(90), || {
            let body = server.https_get(host, "/");
            if body.contains("v2") {
                return true;
            }
            last_v2_body = body;
            false
        }),
        "timed out waiting for v2 response, last body: {}",
        last_v2_body
    );

    // Eventually we should converge back to 1 healthy instance, and status should report v2.
    let mut last_status = serde_json::json!({"status": "no-response"});
    assert!(
        wait_for(Duration::from_secs(90), || {
            let resp = server.send_command(&serde_json::json!({
                "command": "status",
                "app": "test-app",
            }));
            last_status = resp.clone();
            let data = match resp.get("data") {
                Some(d) => d,
                None => return false,
            };

            let version_ok = data.get("version").and_then(|v| v.as_str()) == Some("v2");
            let instances = data.get("instances").and_then(|i| i.as_array());
            let instances = match instances {
                Some(i) => i,
                None => return false,
            };
            if instances.len() != 1 {
                return false;
            }
            let state_ok = instances[0].get("state").and_then(|s| s.as_str()) == Some("healthy");
            version_ok && state_ok
        }),
        "timed out waiting for converged v2 status, last status: {}",
        last_status
    );
}
