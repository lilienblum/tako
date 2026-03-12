use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum Request {
    Ping,
    Info,
    /// Register a persistent app by project directory.
    RegisterApp {
        project_dir: String,
        app_name: String,
        #[serde(default)]
        hosts: Vec<String>,
        upstream_port: u16,
        command: Vec<String>,
        env: std::collections::HashMap<String, String>,
        log_path: String,
        #[serde(default)]
        client_pid: Option<u32>,
    },
    /// Unregister (stop) an app by project directory.
    UnregisterApp {
        project_dir: String,
    },
    /// Update an app's status (running/idle/stopped).
    SetAppStatus {
        project_dir: String,
        status: String,
    },
    /// Hand off a running process PID to the daemon.
    HandoffApp {
        project_dir: String,
        pid: u32,
    },
    /// Request an app restart (relayed to the owning client via events).
    RestartApp {
        project_dir: String,
    },
    /// List all registered apps.
    ListRegisteredApps,
    ListApps,
    SubscribeEvents,
    StopServer,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum Response {
    Pong,
    Apps {
        apps: Vec<AppInfo>,
    },
    Info {
        info: DevInfo,
    },
    AppRegistered {
        app_name: String,
        project_dir: String,
        url: String,
    },
    AppUnregistered {
        project_dir: String,
    },
    AppStatusUpdated {
        project_dir: String,
        status: String,
    },
    AppRestarting {
        project_dir: String,
    },
    AppHandedOff {
        project_dir: String,
    },
    RegisteredApps {
        apps: Vec<RegisteredAppInfo>,
    },
    Subscribed,
    Event {
        event: DevEvent,
    },
    Stopping,
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum DevEvent {
    RequestStarted {
        host: String,
        #[serde(default)]
        path: String,
    },
    RequestFinished {
        host: String,
        #[serde(default)]
        path: String,
    },
    AppStatusChanged {
        project_dir: String,
        app_name: String,
        status: String,
    },
    RestartRequested {
        project_dir: String,
        app_name: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RegisteredAppInfo {
    pub project_dir: String,
    pub app_name: String,
    pub hosts: Vec<String>,
    pub upstream_port: u16,
    pub status: String,
    pub pid: Option<u32>,
    pub client_pid: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppInfo {
    pub app_name: String,
    #[serde(default)]
    pub hosts: Vec<String>,
    pub upstream_port: u16,
    pub pid: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DevInfo {
    /// Where the daemon proxy is currently listening.
    pub listen: String,
    pub port: u16,
    /// IP currently advertised for `.tako.test` hostnames.
    pub advertised_ip: String,
    #[serde(default)]
    pub local_dns_enabled: bool,
    #[serde(default)]
    pub local_dns_port: u16,
    #[serde(default)]
    pub control_clients: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serde_roundtrip_ping_pong() {
        let req = Request::Ping;
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(json, r#"{"type":"Ping"}"#);
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);

        let resp = Response::Pong;
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(json, r#"{"type":"Pong"}"#);
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);
    }

    #[test]
    fn serde_roundtrip_stop() {
        let req = Request::StopServer;
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);

        let resp = Response::Stopping;
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);
    }

    #[test]
    fn serde_roundtrip_events() {
        let req = Request::SubscribeEvents;
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);

        let resp = Response::Subscribed;
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);

        let resp = Response::Event {
            event: DevEvent::RequestStarted {
                host: "a.tako.test".to_string(),
                path: "/api".to_string(),
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);

        let resp = Response::Event {
            event: DevEvent::RequestFinished {
                host: "a.tako.test".to_string(),
                path: "/api".to_string(),
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);
    }

    #[test]
    fn serde_roundtrip_info() {
        let req = Request::Info;
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);

        let resp = Response::Info {
            info: DevInfo {
                listen: "127.0.0.1:8443".to_string(),
                port: 8443,
                advertised_ip: "127.0.0.1".to_string(),
                local_dns_enabled: true,
                local_dns_port: 53535,
                control_clients: 1,
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);
    }

    #[test]
    fn serde_roundtrip_register_app() {
        let req = Request::RegisterApp {
            project_dir: "/home/user/proj".to_string(),
            app_name: "my-app".to_string(),
            hosts: vec![
                "my-app.tako.test".to_string(),
                "my-app.tako.test/api".to_string(),
            ],
            upstream_port: 3000,
            command: vec!["bun".to_string(), "run".to_string(), "index.ts".to_string()],
            env: std::collections::HashMap::from([(
                "NODE_ENV".to_string(),
                "development".to_string(),
            )]),
            log_path: "/home/user/.tako/dev/logs/my-app.jsonl".to_string(),
            client_pid: Some(1234),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);

        let resp = Response::AppRegistered {
            app_name: "my-app".to_string(),
            project_dir: "/home/user/proj".to_string(),
            url: "https://my-app.tako.test/".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);
    }

    #[test]
    fn serde_roundtrip_unregister_app() {
        let req = Request::UnregisterApp {
            project_dir: "/proj".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);

        let resp = Response::AppUnregistered {
            project_dir: "/proj".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);
    }

    #[test]
    fn serde_roundtrip_set_app_status() {
        let req = Request::SetAppStatus {
            project_dir: "/proj".to_string(),
            status: "idle".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);

        let resp = Response::AppStatusUpdated {
            project_dir: "/proj".to_string(),
            status: "idle".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);
    }

    #[test]
    fn serde_roundtrip_handoff_app() {
        let req = Request::HandoffApp {
            project_dir: "/proj".to_string(),
            pid: 12345,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);

        let resp = Response::AppHandedOff {
            project_dir: "/proj".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);
    }

    #[test]
    fn serde_roundtrip_list_registered_apps() {
        let req = Request::ListRegisteredApps;
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);

        let resp = Response::RegisteredApps {
            apps: vec![RegisteredAppInfo {
                project_dir: "/proj".to_string(),
                app_name: "app".to_string(),
                hosts: vec!["app.tako.test".to_string()],
                upstream_port: 3000,
                status: "running".to_string(),
                pid: Some(111),
                client_pid: Some(222),
            }],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);
    }

    #[test]
    fn serde_roundtrip_app_status_changed_event() {
        let resp = Response::Event {
            event: DevEvent::AppStatusChanged {
                project_dir: "/proj".to_string(),
                app_name: "app".to_string(),
                status: "idle".to_string(),
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);
    }

    #[test]
    fn serde_roundtrip_restart_app() {
        let req = Request::RestartApp {
            project_dir: "/proj".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);

        let resp = Response::AppRestarting {
            project_dir: "/proj".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);
    }

    #[test]
    fn serde_roundtrip_restart_requested_event() {
        let resp = Response::Event {
            event: DevEvent::RestartRequested {
                project_dir: "/proj".to_string(),
                app_name: "app".to_string(),
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);
    }

    #[test]
    fn serde_event_defaults_path_to_empty_for_older_payloads() {
        // Old dev-server sends RequestStarted without path field.
        let json = r#"{"type":"Event","event":{"type":"RequestStarted","host":"a.tako.test"}}"#;
        let resp: Response = serde_json::from_str(json).unwrap();
        match resp {
            Response::Event {
                event: DevEvent::RequestStarted { host, path },
            } => {
                assert_eq!(host, "a.tako.test");
                assert_eq!(path, "");
            }
            other => panic!("unexpected: {other:?}"),
        }
    }

}
