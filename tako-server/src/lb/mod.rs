//! Load balancer - routes requests to healthy instances
//!
//! Features:
//! - Round-robin load balancing
//! - Least-connections balancing
//! - IP hash for sticky sessions
//! - Health-aware routing
//! - On-demand instance spawning

use crate::instances::{App, AppManager, Instance};
use crate::socket::InstanceState;
use dashmap::DashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::net::IpAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Load balancing strategy
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Strategy {
    /// Distribute requests evenly across instances
    #[default]
    RoundRobin,
    /// Send to instance with fewest active connections
    LeastConnections,
    /// Sticky sessions based on IP hash
    IpHash,
}

/// Load balancer for a single app
pub struct AppLoadBalancer {
    /// App reference
    app: Arc<App>,
    /// Load balancing strategy
    strategy: Strategy,
    /// Round-robin counter
    rr_counter: AtomicUsize,
    /// Active connections per instance
    connections: DashMap<u32, AtomicU64>,
}

impl AppLoadBalancer {
    pub fn new(app: Arc<App>, strategy: Strategy) -> Self {
        Self {
            app,
            strategy,
            rr_counter: AtomicUsize::new(0),
            connections: DashMap::new(),
        }
    }

    /// Get an instance to handle a request
    pub fn get_instance(&self) -> Option<Arc<Instance>> {
        self.get_instance_for_ip(None)
    }

    /// Get an instance to handle a request, with optional client IP for sticky sessions
    pub fn get_instance_for_ip(&self, client_ip: Option<IpAddr>) -> Option<Arc<Instance>> {
        match self.strategy {
            Strategy::RoundRobin => self.round_robin(),
            Strategy::LeastConnections => self.least_connections(),
            Strategy::IpHash => self.ip_hash(client_ip),
        }
    }

    /// Get instance using round-robin
    fn round_robin(&self) -> Option<Arc<Instance>> {
        let healthy = self.app.get_healthy_instances();
        if healthy.is_empty() {
            return None;
        }

        let idx = self.rr_counter.fetch_add(1, Ordering::Relaxed) % healthy.len();
        healthy.get(idx).cloned()
    }

    /// Get instance with least active connections
    fn least_connections(&self) -> Option<Arc<Instance>> {
        let healthy = self.app.get_healthy_instances();
        if healthy.is_empty() {
            return None;
        }

        healthy.into_iter().min_by_key(|instance| {
            self.connections
                .get(&instance.id)
                .map(|c| c.load(Ordering::Relaxed))
                .unwrap_or(0)
        })
    }

    /// Get instance using IP hash for sticky sessions
    ///
    /// The same client IP will consistently route to the same instance
    /// (as long as the instance remains healthy). If no client IP is
    /// provided, falls back to round-robin.
    fn ip_hash(&self, client_ip: Option<IpAddr>) -> Option<Arc<Instance>> {
        let healthy = self.app.get_healthy_instances();
        if healthy.is_empty() {
            return None;
        }

        // Fall back to round-robin if no IP provided
        let ip = match client_ip {
            Some(ip) => ip,
            None => return self.round_robin(),
        };

        // Hash the IP address
        let mut hasher = DefaultHasher::new();
        ip.hash(&mut hasher);
        let hash = hasher.finish();

        // Use hash to select instance
        let idx = (hash as usize) % healthy.len();
        healthy.get(idx).cloned()
    }

    /// Mark connection started
    pub fn connection_started(&self, instance_id: u32) {
        self.connections
            .entry(instance_id)
            .or_insert_with(|| AtomicU64::new(0))
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Mark connection ended
    pub fn connection_ended(&self, instance_id: u32) {
        if let Some(count) = self.connections.get(&instance_id) {
            count.fetch_sub(1, Ordering::Relaxed);
        }
    }

    /// Get active connection count for an instance
    pub fn active_connections(&self, instance_id: u32) -> u64 {
        self.connections
            .get(&instance_id)
            .map(|c| c.load(Ordering::Relaxed))
            .unwrap_or(0)
    }
}

/// Global load balancer managing all apps
pub struct LoadBalancer {
    /// Per-app load balancers
    app_lbs: DashMap<String, Arc<AppLoadBalancer>>,
    /// App manager reference
    app_manager: Arc<AppManager>,
    /// Default strategy
    default_strategy: Strategy,
}

impl LoadBalancer {
    pub fn new(app_manager: Arc<AppManager>) -> Self {
        Self {
            app_lbs: DashMap::new(),
            app_manager,
            default_strategy: Strategy::RoundRobin,
        }
    }

    /// Register an app with the load balancer
    pub fn register_app(&self, app: Arc<App>) {
        let name = app.name();
        let lb = Arc::new(AppLoadBalancer::new(app, self.default_strategy));
        self.app_lbs.insert(name, lb);
    }

    /// Remove an app from the load balancer
    pub fn unregister_app(&self, name: &str) {
        self.app_lbs.remove(name);
    }

    /// Get a backend instance for a request
    pub fn get_backend(&self, app_name: &str) -> Option<Backend> {
        self.get_backend_for_ip(app_name, None)
    }

    /// Get a backend instance for a request, with optional client IP for sticky sessions
    pub fn get_backend_for_ip(&self, app_name: &str, client_ip: Option<IpAddr>) -> Option<Backend> {
        let lb = self.app_lbs.get(app_name)?;
        let instance = lb.get_instance_for_ip(client_ip)?;

        lb.connection_started(instance.id);

        Some(Backend {
            app_name: app_name.to_string(),
            instance_id: instance.id,
            addr: format!("127.0.0.1:{}", instance.port),
        })
    }

    /// Mark request completed
    pub fn request_completed(&self, app_name: &str, instance_id: u32) {
        if let Some(lb) = self.app_lbs.get(app_name) {
            lb.connection_ended(instance_id);
        }
    }

    /// Check if any healthy instance exists
    pub fn has_healthy_instance(&self, app_name: &str) -> bool {
        self.app_lbs
            .get(app_name)
            .map(|lb| lb.app.count_by_state(InstanceState::Healthy) > 0)
            .unwrap_or(false)
    }

    /// Get app manager
    pub fn app_manager(&self) -> &Arc<AppManager> {
        &self.app_manager
    }
}

/// A selected backend for a request
#[derive(Debug, Clone)]
pub struct Backend {
    /// App name
    pub app_name: String,
    /// Instance ID
    pub instance_id: u32,
    /// Address to connect to (e.g., "127.0.0.1:3000")
    pub addr: String,
}

impl Backend {
    /// Parse address into host and port
    pub fn host_port(&self) -> (&str, u16) {
        let parts: Vec<&str> = self.addr.split(':').collect();
        let host = parts.first().copied().unwrap_or("127.0.0.1");
        let port = parts.get(1).and_then(|p| p.parse().ok()).unwrap_or(3000);
        (host, port)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::instances::AppConfig;
    use tokio::sync::mpsc;

    fn create_test_app() -> Arc<App> {
        let (tx, _rx) = mpsc::channel(16);
        let config = AppConfig {
            name: "test-app".to_string(),
            base_port: 3000,
            ..Default::default()
        };
        Arc::new(App::new(config, tx))
    }

    #[test]
    fn test_round_robin() {
        let app = create_test_app();

        // Allocate 3 instances and mark them healthy
        let i1 = app.allocate_instance();
        let i2 = app.allocate_instance();
        let i3 = app.allocate_instance();
        i1.set_state(InstanceState::Healthy);
        i2.set_state(InstanceState::Healthy);
        i3.set_state(InstanceState::Healthy);

        let lb = AppLoadBalancer::new(app, Strategy::RoundRobin);

        // Should cycle through instances
        let mut ports = vec![];
        for _ in 0..6 {
            let instance = lb.get_instance().unwrap();
            ports.push(instance.port);
        }

        // Should see each port twice
        assert_eq!(ports.iter().filter(|&&p| p == 3000).count(), 2);
        assert_eq!(ports.iter().filter(|&&p| p == 3001).count(), 2);
        assert_eq!(ports.iter().filter(|&&p| p == 3002).count(), 2);
    }

    #[test]
    fn test_least_connections() {
        let app = create_test_app();

        let i1 = app.allocate_instance();
        let i2 = app.allocate_instance();
        i1.set_state(InstanceState::Healthy);
        i2.set_state(InstanceState::Healthy);

        let lb = AppLoadBalancer::new(app, Strategy::LeastConnections);

        // Both have 0 connections, should get first one
        let instance = lb.get_instance().unwrap();
        lb.connection_started(instance.id);

        // Now first has 1 connection, should get second
        let instance2 = lb.get_instance().unwrap();
        assert_ne!(instance.id, instance2.id);
    }

    #[test]
    fn test_connection_tracking() {
        let app = create_test_app();
        let i1 = app.allocate_instance();
        i1.set_state(InstanceState::Healthy);

        let lb = AppLoadBalancer::new(app, Strategy::RoundRobin);

        assert_eq!(lb.active_connections(1), 0);

        lb.connection_started(1);
        lb.connection_started(1);
        assert_eq!(lb.active_connections(1), 2);

        lb.connection_ended(1);
        assert_eq!(lb.active_connections(1), 1);
    }

    #[test]
    fn test_no_healthy_instances() {
        let app = create_test_app();
        let i1 = app.allocate_instance();
        i1.set_state(InstanceState::Starting); // Not healthy yet

        let lb = AppLoadBalancer::new(app, Strategy::RoundRobin);
        assert!(lb.get_instance().is_none());
    }

    #[test]
    fn test_backend_host_port() {
        let backend = Backend {
            app_name: "test".to_string(),
            instance_id: 1,
            addr: "127.0.0.1:3000".to_string(),
        };

        let (host, port) = backend.host_port();
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 3000);
    }

    #[test]
    fn test_global_load_balancer() {
        let manager = Arc::new(AppManager::new());
        let lb = LoadBalancer::new(manager.clone());

        let config = AppConfig {
            name: "my-app".to_string(),
            base_port: 4000,
            ..Default::default()
        };
        let app = manager.register_app(config);

        // Allocate and make healthy
        let instance = app.allocate_instance();
        instance.set_state(InstanceState::Healthy);

        lb.register_app(app);

        assert!(lb.has_healthy_instance("my-app"));

        let backend = lb.get_backend("my-app").unwrap();
        assert_eq!(backend.app_name, "my-app");
        assert_eq!(backend.addr, "127.0.0.1:4000");
    }

    #[test]
    fn test_ip_hash_sticky_sessions() {
        let app = create_test_app();

        // Allocate 3 instances and mark them healthy
        let i1 = app.allocate_instance();
        let i2 = app.allocate_instance();
        let i3 = app.allocate_instance();
        i1.set_state(InstanceState::Healthy);
        i2.set_state(InstanceState::Healthy);
        i3.set_state(InstanceState::Healthy);

        let lb = AppLoadBalancer::new(app, Strategy::IpHash);

        // Same IP should always get the same instance
        let ip1: IpAddr = "192.168.1.100".parse().unwrap();
        let ip2: IpAddr = "192.168.1.200".parse().unwrap();

        // Get instance for IP1 multiple times - should be consistent
        let instance_for_ip1_first = lb.get_instance_for_ip(Some(ip1)).unwrap();
        let instance_for_ip1_second = lb.get_instance_for_ip(Some(ip1)).unwrap();
        let instance_for_ip1_third = lb.get_instance_for_ip(Some(ip1)).unwrap();

        assert_eq!(instance_for_ip1_first.id, instance_for_ip1_second.id);
        assert_eq!(instance_for_ip1_second.id, instance_for_ip1_third.id);

        // Get instance for IP2 multiple times - should also be consistent
        let instance_for_ip2_first = lb.get_instance_for_ip(Some(ip2)).unwrap();
        let instance_for_ip2_second = lb.get_instance_for_ip(Some(ip2)).unwrap();

        assert_eq!(instance_for_ip2_first.id, instance_for_ip2_second.id);
    }

    #[test]
    fn test_ip_hash_different_ips_distribute() {
        let app = create_test_app();

        // Allocate 3 instances and mark them healthy
        let i1 = app.allocate_instance();
        let i2 = app.allocate_instance();
        let i3 = app.allocate_instance();
        i1.set_state(InstanceState::Healthy);
        i2.set_state(InstanceState::Healthy);
        i3.set_state(InstanceState::Healthy);

        let lb = AppLoadBalancer::new(app, Strategy::IpHash);

        // Test with many different IPs - should distribute across instances
        let mut instance_counts = std::collections::HashMap::new();
        for i in 0..100 {
            let ip: IpAddr = format!("10.0.0.{}", i).parse().unwrap();
            let instance = lb.get_instance_for_ip(Some(ip)).unwrap();
            *instance_counts.entry(instance.id).or_insert(0) += 1;
        }

        // Should have distributed across all 3 instances
        assert_eq!(instance_counts.len(), 3);
        // Each instance should have gotten at least some requests
        for count in instance_counts.values() {
            assert!(*count > 0);
        }
    }

    #[test]
    fn test_ip_hash_fallback_to_round_robin() {
        let app = create_test_app();

        let i1 = app.allocate_instance();
        let i2 = app.allocate_instance();
        i1.set_state(InstanceState::Healthy);
        i2.set_state(InstanceState::Healthy);

        let lb = AppLoadBalancer::new(app, Strategy::IpHash);

        // Without IP, should fall back to round-robin behavior
        let instance1 = lb.get_instance_for_ip(None).unwrap();
        let instance2 = lb.get_instance_for_ip(None).unwrap();

        // Round-robin should cycle through
        assert_ne!(instance1.id, instance2.id);
    }

    #[test]
    fn test_ip_hash_ipv6() {
        let app = create_test_app();

        let i1 = app.allocate_instance();
        let i2 = app.allocate_instance();
        i1.set_state(InstanceState::Healthy);
        i2.set_state(InstanceState::Healthy);

        let lb = AppLoadBalancer::new(app, Strategy::IpHash);

        // Test with IPv6 address
        let ipv6: IpAddr = "2001:db8::1".parse().unwrap();

        let instance1 = lb.get_instance_for_ip(Some(ipv6)).unwrap();
        let instance2 = lb.get_instance_for_ip(Some(ipv6)).unwrap();

        // Same IPv6 should get same instance
        assert_eq!(instance1.id, instance2.id);
    }
}
