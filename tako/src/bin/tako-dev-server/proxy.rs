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
    apps: Arc<Mutex<HashMap<String, AppRoute>>>,
}

#[derive(Clone)]
struct AppRoute {
    upstream_port: u16,
    active: bool,
    notify: Arc<Notify>,
}

impl Routes {
    pub fn set(&self, host: String, app_id: String, upstream_port: u16, active: bool) {
        self.by_host.lock().unwrap().insert(host, app_id.clone());

        let mut apps = self.apps.lock().unwrap();
        let entry = apps.entry(app_id).or_insert_with(|| AppRoute {
            upstream_port,
            active,
            notify: Arc::new(Notify::new()),
        });
        entry.upstream_port = upstream_port;
        entry.active = active;
        if active {
            entry.notify.notify_waiters();
        }
        drop(apps);
    }

    pub fn remove(&self, host: &str) {
        let mut by_host = self.by_host.lock().unwrap();
        let Some(app_id) = by_host.remove(host) else {
            return;
        };
        // Garbage collect app entry if no hosts refer to it.
        let hosts_left = by_host.values().any(|v| v == &app_id);
        drop(by_host);
        if !hosts_left {
            self.apps.lock().unwrap().remove(&app_id);
        }
    }

    pub fn set_active(&self, app_id: &str, active: bool) {
        if let Some(r) = self.apps.lock().unwrap().get_mut(app_id) {
            r.active = active;
            if active {
                r.notify.notify_waiters();
            }
        }
    }

    pub fn lookup(&self, host: &str) -> Option<(String, u16, bool, Arc<Notify>)> {
        let by_host = self.by_host.lock().unwrap();
        let app_id = by_host
            .get(host)
            .or_else(|| {
                // Wildcard fallback: strip the first label and look up `*.{rest}`.
                let rest = host.split_once('.')?.1;
                let wildcard = format!("*.{rest}");
                by_host.get(&wildcard)
            })
            .cloned()?;
        drop(by_host);
        let apps = self.apps.lock().unwrap();
        let r = apps.get(&app_id)?.clone();
        Some((app_id, r.upstream_port, r.active, r.notify))
    }

    pub fn hosts(&self) -> Vec<String> {
        self.by_host.lock().unwrap().keys().cloned().collect()
    }

    pub async fn wait_for_active(&self, app_id: &str, timeout: std::time::Duration) -> bool {
        let notify = {
            let apps = self.apps.lock().unwrap();
            let Some(r) = apps.get(app_id) else {
                return false;
            };
            if r.active {
                return true;
            }
            r.notify.clone()
        };

        // Register interest before awaiting so a notify_waiters() that fires
        // between the lock release above and the await below is not lost.
        let notified = notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        let _ = tokio::time::timeout(timeout, notified).await;
        let apps = self.apps.lock().unwrap();
        apps.get(app_id).is_some_and(|r| r.active)
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

        let Some((app_id, port, active, _notify)) = self.routes.lookup(&hostname) else {
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
                .wait_for_active(&app_id, std::time::Duration::from_secs(30))
                .await;
            if !ready {
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

    #[test]
    fn lookup_matches_wildcard_route() {
        let routes = Routes::default();
        routes.set("*.app.tako".to_string(), "app".to_string(), 3000, true);

        // Exact wildcard key doesn't match a real request.
        let hit = routes.lookup("foo.app.tako");
        assert!(hit.is_some());
        let (app_id, port, active, _) = hit.unwrap();
        assert_eq!(app_id, "app");
        assert_eq!(port, 3000);
        assert!(active);

        // Unrelated host should not match.
        assert!(routes.lookup("foo.other.tako").is_none());
    }

    #[tokio::test]
    async fn routes_waits_for_active() {
        let routes = Routes::default();
        routes.set("a.tako".to_string(), "app".to_string(), 1234, false);

        let r2 = routes.clone();
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            r2.set_active("app", true);
        });

        assert!(
            routes
                .wait_for_active("app", std::time::Duration::from_secs(1))
                .await
        );
    }
}
