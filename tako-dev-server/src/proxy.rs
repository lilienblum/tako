use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use pingora_core::Result;
use pingora_core::upstreams::peer::HttpPeer;
use pingora_http::ResponseHeader;
use pingora_proxy::{ProxyHttp, Session};
use tokio::sync::Notify;

use crate::protocol;

#[derive(Clone, Default)]
pub struct Routes {
    by_host: Arc<Mutex<HashMap<String, String>>>,
    leases: Arc<Mutex<HashMap<String, LeaseRoute>>>,
}

#[derive(Clone)]
struct LeaseRoute {
    upstream_port: u16,
    active: bool,
    notify: Arc<Notify>,
}

impl Routes {
    pub fn set(&self, host: String, lease_id: String, upstream_port: u16, active: bool) {
        self.by_host.lock().unwrap().insert(host, lease_id.clone());

        let mut leases = self.leases.lock().unwrap();
        let entry = leases.entry(lease_id).or_insert_with(|| LeaseRoute {
            upstream_port,
            active,
            notify: Arc::new(Notify::new()),
        });
        entry.upstream_port = upstream_port;
        entry.active = active;
        if active {
            entry.notify.notify_waiters();
        }
    }

    pub fn remove(&self, host: &str) {
        let lease_id = self.by_host.lock().unwrap().remove(host);
        let Some(lease_id) = lease_id else { return };

        // Garbage collect lease entry if no hosts refer to it.
        let mut hosts_left = false;
        for v in self.by_host.lock().unwrap().values() {
            if v == &lease_id {
                hosts_left = true;
                break;
            }
        }
        if !hosts_left {
            self.leases.lock().unwrap().remove(&lease_id);
        }
    }

    pub fn set_active(&self, lease_id: &str, active: bool) {
        if let Some(l) = self.leases.lock().unwrap().get_mut(lease_id) {
            l.active = active;
            if active {
                l.notify.notify_waiters();
            }
        }
    }

    pub fn lookup(&self, host: &str) -> Option<(String, u16, bool, Arc<Notify>)> {
        let lease_id = self.by_host.lock().unwrap().get(host).cloned()?;
        let leases = self.leases.lock().unwrap();
        let l = leases.get(&lease_id)?.clone();
        Some((lease_id, l.upstream_port, l.active, l.notify))
    }

    pub fn hosts(&self) -> Vec<String> {
        self.by_host.lock().unwrap().keys().cloned().collect()
    }

    pub async fn wait_for_active(&self, lease_id: &str, timeout: std::time::Duration) -> bool {
        let notify = {
            let leases = self.leases.lock().unwrap();
            let Some(l) = leases.get(lease_id) else {
                return false;
            };
            if l.active {
                return true;
            }
            l.notify.clone()
        };

        let _ = tokio::time::timeout(timeout, notify.notified()).await;
        let leases = self.leases.lock().unwrap();
        leases.get(lease_id).is_some_and(|l| l.active)
    }
}

#[derive(Clone)]
pub struct DevProxy {
    pub routes: Routes,
    pub events: tokio::sync::mpsc::UnboundedSender<protocol::DevEvent>,
}

#[derive(Default)]
pub struct Ctx {
    upstream_port: Option<u16>,
    host: Option<String>,
}

#[async_trait]
impl ProxyHttp for DevProxy {
    type CTX = Ctx;

    fn new_ctx(&self) -> Self::CTX {
        Ctx::default()
    }

    async fn request_filter(&self, session: &mut Session, ctx: &mut Self::CTX) -> Result<bool> {
        let host_header = {
            let req = session.req_header();
            req.headers
                .get("host")
                .and_then(|h| h.to_str().ok())
                .unwrap_or("")
                .to_string()
        };

        let hostname = host_header
            .split(':')
            .next()
            .unwrap_or(host_header.as_str())
            .to_string();
        ctx.host = Some(hostname.clone());

        let _ = self.events.send(protocol::DevEvent::RequestStarted {
            host: hostname.clone(),
        });

        let Some((lease_id, port, active, _notify)) = self.routes.lookup(&hostname) else {
            let mut header = ResponseHeader::build(404, None)?;
            header.insert_header("Content-Type", "text/plain")?;
            session
                .write_response_header(Box::new(header), false)
                .await?;

            let mut known = self.routes.hosts();
            known.sort();
            session
                .write_response_body(
                    Some(
                        format!(
                            "Unknown dev host '{}'. Known apps:\n{}",
                            hostname,
                            known.join("\n")
                        )
                        .into(),
                    ),
                    true,
                )
                .await?;
            return Ok(true);
        };

        if !active {
            let ready = self
                .routes
                .wait_for_active(&lease_id, std::time::Duration::from_secs(30))
                .await;
            if !ready {
                let _ = self
                    .events
                    .send(protocol::DevEvent::RequestFinished { host: hostname });
                let mut header = ResponseHeader::build(503, None)?;
                header.insert_header("Content-Type", "text/plain")?;
                session
                    .write_response_header(Box::new(header), false)
                    .await?;
                session
                    .write_response_body(Some("Starting...".into()), true)
                    .await?;
                return Ok(true);
            }
        }

        ctx.upstream_port = Some(port);
        Ok(false)
    }

    async fn logging(
        &self,
        _session: &mut Session,
        _e: Option<&pingora_core::Error>,
        ctx: &mut Self::CTX,
    ) where
        Self::CTX: Send + Sync,
    {
        if let Some(host) = ctx.host.take() {
            let _ = self
                .events
                .send(protocol::DevEvent::RequestFinished { host });
        }
    }

    async fn upstream_peer(
        &self,
        _session: &mut Session,
        ctx: &mut Self::CTX,
    ) -> Result<Box<HttpPeer>> {
        let port = ctx
            .upstream_port
            .ok_or_else(|| pingora_core::Error::new(pingora_core::ErrorType::ConnectNoRoute))?;
        let peer = HttpPeer::new(("127.0.0.1".to_string(), port), false, String::new());
        Ok(Box::new(peer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn routes_waits_for_active() {
        let routes = Routes::default();
        routes.set("a.tako.local".to_string(), "lease".to_string(), 1234, false);

        let r2 = routes.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            r2.set_active("lease", true);
        });

        assert!(
            routes
                .wait_for_active("lease", std::time::Duration::from_secs(1))
                .await
        );
    }
}
