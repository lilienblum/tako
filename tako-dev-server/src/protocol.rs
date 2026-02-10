use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum Request {
    Ping,
    Info,
    GetToken,
    /// Register (or refresh) a lease for an app.
    ///
    /// - `app_name` must be a DNS-safe label (used for `{app}.tako.local`).
    /// - `upstream_port` is where the app is listening on the host.
    /// - `ttl_ms` is the lease TTL; the client must renew before it expires.
    RegisterLease {
        token: String,
        app_name: String,
        /// Hostnames to register for this app.
        /// If empty, defaults to `{app_name}.tako.local`.
        #[serde(default)]
        hosts: Vec<String>,
        upstream_port: u16,
        #[serde(default)]
        active: bool,
        ttl_ms: u64,
    },
    SetLeaseActive {
        token: String,
        lease_id: String,
        active: bool,
    },
    RenewLease {
        token: String,
        lease_id: String,
        ttl_ms: u64,
    },
    UnregisterLease {
        token: String,
        lease_id: String,
    },
    ListApps,
    SubscribeEvents {
        token: String,
    },
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
    Token {
        token: String,
    },
    LeaseRegistered {
        app_name: String,
        lease_id: String,
        expires_in_ms: u64,
        url: String,
    },
    LeaseRenewed {
        lease_id: String,
        expires_in_ms: u64,
    },
    LeaseUnregistered {
        lease_id: String,
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
    RequestStarted { host: String },
    RequestFinished { host: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppInfo {
    /// Opaque lease identifier.
    #[serde(default)]
    pub lease_id: String,
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
    /// IP currently advertised for `.tako.local` hostnames.
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
    fn serde_roundtrip_up_app_and_list() {
        let req = Request::RegisterLease {
            token: "t".to_string(),
            app_name: "my-app".to_string(),
            hosts: vec!["my-app.tako.local".to_string()],
            upstream_port: 1234,
            active: true,
            ttl_ms: 30_000,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);

        let resp = Response::LeaseRegistered {
            app_name: "my-app".to_string(),
            lease_id: "lease".to_string(),
            expires_in_ms: 30_000,
            url: "https://my-app.tako.local/".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);

        let resp = Response::Apps {
            apps: vec![AppInfo {
                lease_id: "lease".to_string(),
                app_name: "a".to_string(),
                hosts: vec!["a.tako.local".to_string()],
                upstream_port: 1234,
                pid: Some(1),
            }],
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);
    }

    #[test]
    fn serde_roundtrip_down_and_stop() {
        let req = Request::GetToken;
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);

        let resp = Response::Token {
            token: "t".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);

        let req = Request::StopServer;
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);

        let resp = Response::Stopping;
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);
    }

    #[test]
    fn serde_roundtrip_logs_requests() {
        let req = Request::RenewLease {
            token: "t".to_string(),
            lease_id: "lease".to_string(),
            ttl_ms: 30_000,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);

        let req = Request::UnregisterLease {
            token: "t".to_string(),
            lease_id: "lease".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);

        let resp = Response::LeaseRenewed {
            lease_id: "lease".to_string(),
            expires_in_ms: 30_000,
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);

        let resp = Response::LeaseUnregistered {
            lease_id: "lease".to_string(),
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);
    }

    #[test]
    fn serde_roundtrip_events() {
        let req = Request::SubscribeEvents {
            token: "t".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);

        let req = Request::SetLeaseActive {
            token: "t".to_string(),
            lease_id: "lease".to_string(),
            active: true,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert_eq!(serde_json::from_str::<Request>(&json).unwrap(), req);

        let resp = Response::Subscribed;
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);

        let resp = Response::Event {
            event: DevEvent::RequestStarted {
                host: "a.tako.local".to_string(),
            },
        };
        let json = serde_json::to_string(&resp).unwrap();
        assert_eq!(serde_json::from_str::<Response>(&json).unwrap(), resp);

        let resp = Response::Event {
            event: DevEvent::RequestFinished {
                host: "a.tako.local".to_string(),
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
    fn serde_info_defaults_control_clients_to_zero_for_older_payloads() {
        let json = r#"{"type":"Info","info":{"listen":"127.0.0.1:8443","port":8443,"advertised_ip":"127.0.0.1","local_dns_enabled":true,"local_dns_port":53535}}"#;
        let resp: Response = serde_json::from_str(json).unwrap();
        match resp {
            Response::Info { info } => assert_eq!(info.control_clients, 0),
            other => panic!("unexpected: {other:?}"),
        }
    }
}
