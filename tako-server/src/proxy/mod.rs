//! HTTP/HTTPS Proxy using Pingora
//!
//! Routes incoming HTTP requests to app instances based on Host header.
//! Supports TLS termination with automatic certificate management.
//! Handles ACME HTTP-01 challenges for Let's Encrypt certificate issuance.

mod request;
mod server;
mod service;
mod static_files;

#[allow(unused_imports)]
pub use server::{ProxyBuilder, TlsConfig, build_server, build_server_with_acme};
#[allow(unused_imports)]
pub use static_files::*;

use crate::lb::LoadBalancer;
use crate::routing::RouteTable;
use crate::scaling::ColdStartManager;
use crate::tls::{ChallengeHandler, ChallengeTokens};
use parking_lot::RwLock as SyncRwLock;
use pingora_cache::MemCache;
use pingora_cache::eviction::simple_lru;
use pingora_cache::lock::{CacheKeyLockImpl, CacheLock};
use std::collections::HashMap;
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering as AtomicOrdering};
use std::time::Duration;
use tokio::sync::RwLock;

#[cfg(test)]
use pingora_http::{RequestHeader, ResponseHeader};
#[cfg(test)]
use pingora_proxy::ProxyHttp;
#[cfg(test)]
use request::{
    build_proxy_cache_key, insert_body_headers, is_effective_request_https,
    path_looks_like_static_asset, request_is_proxy_cacheable, response_cacheability,
    should_assume_forwarded_private_request_https, should_redirect_http_request,
    static_lookup_paths,
};
#[cfg(test)]
use service::BackendResolution;

/// Proxy configuration
#[derive(Debug, Clone)]
pub struct ProxyConfig {
    /// HTTP port to listen on
    pub http_port: u16,
    /// HTTPS port to listen on
    pub https_port: u16,
    /// Whether to enable HTTPS
    pub enable_https: bool,
    /// Whether to use self-signed certs for development
    pub dev_mode: bool,
    /// Directory for certificates
    pub cert_dir: PathBuf,
    /// Whether to redirect HTTP to HTTPS
    pub redirect_http_to_https: bool,
    /// Optional upstream response cache configuration
    pub response_cache: Option<ResponseCacheConfig>,
    /// Optional Prometheus metrics port (localhost-only listener)
    pub metrics_port: Option<u16>,
}

/// Upstream response cache configuration
#[derive(Debug, Clone)]
pub struct ResponseCacheConfig {
    /// Total cache capacity tracked by the LRU eviction manager
    pub max_size_bytes: usize,
    /// Maximum cacheable response body size per object
    pub max_file_size_bytes: usize,
    /// Cache lock timeout to collapse concurrent misses for the same key
    pub lock_timeout: Duration,
}

impl Default for ResponseCacheConfig {
    fn default() -> Self {
        Self {
            max_size_bytes: 256 * 1024 * 1024,    // 256 MiB
            max_file_size_bytes: 8 * 1024 * 1024, // 8 MiB
            lock_timeout: Duration::from_secs(5),
        }
    }
}

#[derive(Clone, Copy)]
struct ResponseCacheRuntime {
    storage: &'static MemCache,
    eviction: &'static simple_lru::Manager,
    cache_lock: &'static CacheKeyLockImpl,
    max_file_size_bytes: usize,
}

impl ResponseCacheRuntime {
    fn new(config: &ResponseCacheConfig) -> Self {
        let storage = Box::leak(Box::new(MemCache::new()));
        let eviction = Box::leak(Box::new(simple_lru::Manager::new(config.max_size_bytes)));
        let cache_lock = Box::leak(Box::new(CacheLock::new(config.lock_timeout)));
        let cache_lock: &'static CacheKeyLockImpl = cache_lock;
        Self {
            storage,
            eviction,
            cache_lock,
            max_file_size_bytes: config.max_file_size_bytes,
        }
    }
}

impl Default for ProxyConfig {
    fn default() -> Self {
        Self {
            http_port: 80,
            https_port: 443,
            enable_https: true,
            dev_mode: false,
            cert_dir: PathBuf::from("/opt/tako/certs"),
            redirect_http_to_https: true,
            response_cache: Some(ResponseCacheConfig::default()),
            metrics_port: Some(9898),
        }
    }
}

impl ProxyConfig {
    /// Create development configuration
    pub fn development() -> Self {
        Self {
            http_port: 8080,
            https_port: 8443,
            enable_https: true,
            dev_mode: true,
            cert_dir: PathBuf::from("./data/certs"),
            redirect_http_to_https: true,
            response_cache: Some(ResponseCacheConfig::default()),
            metrics_port: Some(9898),
        }
    }
}

/// Maximum concurrent requests from a single IP address.
const MAX_REQUESTS_PER_IP: u32 = 2048;

/// Maximum request body size (128 MiB). Requests exceeding this are rejected
/// with 413 Payload Too Large to prevent memory/disk exhaustion attacks.
const MAX_REQUEST_BODY_BYTES: u64 = 128 * 1024 * 1024;

/// Per-IP concurrent request tracker for basic DDoS mitigation.
/// Tracks in-flight requests per client IP with O(1) increment/decrement.
struct IpRequestTracker {
    connections: dashmap::DashMap<IpAddr, AtomicU32>,
}

impl IpRequestTracker {
    fn new() -> Self {
        Self {
            connections: dashmap::DashMap::new(),
        }
    }

    /// Try to acquire a slot for this IP. Returns false if over the limit.
    fn try_acquire(&self, ip: IpAddr) -> bool {
        let entry = self
            .connections
            .entry(ip)
            .or_insert_with(|| AtomicU32::new(0));
        let prev = entry.value().fetch_add(1, AtomicOrdering::Relaxed);
        if prev >= MAX_REQUESTS_PER_IP {
            entry.value().fetch_sub(1, AtomicOrdering::Relaxed);
            false
        } else {
            true
        }
    }

    /// Release a slot for this IP.
    fn release(&self, ip: IpAddr) {
        if let Some(entry) = self.connections.get(&ip) {
            // Use compare-exchange loop to prevent underflow: two concurrent
            // release calls that both see count=1 would otherwise both
            // decrement, wrapping to u32::MAX and permanently locking the IP.
            loop {
                let current = entry.value().load(AtomicOrdering::Relaxed);
                if current == 0 {
                    return;
                }
                if entry
                    .value()
                    .compare_exchange_weak(
                        current,
                        current - 1,
                        AtomicOrdering::Relaxed,
                        AtomicOrdering::Relaxed,
                    )
                    .is_ok()
                {
                    // Clean up zero-count entries to prevent unbounded map growth.
                    if current == 1 {
                        drop(entry);
                        self.connections
                            .remove_if(&ip, |_, v| v.load(AtomicOrdering::Relaxed) == 0);
                    }
                    return;
                }
            }
        }
    }
}

/// Tako HTTP proxy service
pub struct TakoProxy {
    /// Load balancer
    lb: Arc<LoadBalancer>,
    /// Route table (app_name -> route patterns)
    routes: Arc<RwLock<RouteTable>>,
    /// Configuration
    config: ProxyConfig,
    /// ACME challenge handler (optional)
    challenge_handler: Option<ChallengeHandler>,

    /// Cold start coordinator for on-demand apps
    cold_start: Arc<ColdStartManager>,
    /// Shared upstream response cache runtime (optional)
    response_cache: Option<ResponseCacheRuntime>,
    /// Reused per-app static file server state for hot path requests
    static_servers: SyncRwLock<HashMap<String, Arc<AppStaticServer>>>,
    /// Per-IP concurrent request limiter (DDoS mitigation)
    ip_tracker: IpRequestTracker,
}

impl TakoProxy {
    pub fn new(
        lb: Arc<LoadBalancer>,
        routes: Arc<RwLock<RouteTable>>,
        config: ProxyConfig,
        cold_start: Arc<ColdStartManager>,
    ) -> Self {
        let response_cache = config
            .response_cache
            .as_ref()
            .map(ResponseCacheRuntime::new);
        Self {
            lb,
            routes,
            config,
            challenge_handler: None,
            cold_start,
            response_cache,
            static_servers: SyncRwLock::new(HashMap::new()),
            ip_tracker: IpRequestTracker::new(),
        }
    }

    /// Create proxy with ACME challenge handling
    pub fn with_acme(
        lb: Arc<LoadBalancer>,
        routes: Arc<RwLock<RouteTable>>,
        config: ProxyConfig,
        tokens: ChallengeTokens,
        cold_start: Arc<ColdStartManager>,
    ) -> Self {
        let response_cache = config
            .response_cache
            .as_ref()
            .map(ResponseCacheRuntime::new);
        Self {
            lb,
            routes,
            config,
            challenge_handler: Some(ChallengeHandler::new(tokens)),
            cold_start,
            response_cache,
            static_servers: SyncRwLock::new(HashMap::new()),
            ip_tracker: IpRequestTracker::new(),
        }
    }

    /// Get config
    pub fn config(&self) -> &ProxyConfig {
        &self.config
    }
}

#[cfg(test)]
mod tests;
