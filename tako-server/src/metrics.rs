//! Prometheus metrics for the Tako proxy.
//!
//! Metrics are registered with the global Prometheus registry and served via
//! Pingora's built-in PrometheusServer on a dedicated listener port.
//!
//! All metrics carry a `server` label (machine hostname) so multi-server
//! deployments are distinguishable in Grafana/Datadog without relying on
//! scraper-side relabeling.

use prometheus::{
    HistogramOpts, HistogramVec, IntCounterVec, IntGaugeVec, Opts, register_histogram_vec,
    register_int_counter_vec, register_int_gauge_vec,
};
use std::sync::{LazyLock, OnceLock};
use std::time::Instant;

/// Server hostname, set once at startup via `init()`.
static SERVER_LABEL: OnceLock<String> = OnceLock::new();

fn server() -> &'static str {
    SERVER_LABEL.get().map(|s| s.as_str()).unwrap_or("unknown")
}

/// Total HTTP requests handled by the proxy.
pub static HTTP_REQUESTS_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        Opts::new("tako_http_requests_total", "Total HTTP requests proxied"),
        &["server", "app", "status"]
    )
    .unwrap()
});

/// Request duration in seconds.
pub static HTTP_REQUEST_DURATION_SECONDS: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!(
        HistogramOpts::new(
            "tako_http_request_duration_seconds",
            "HTTP request duration in seconds"
        )
        .buckets(vec![
            0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
        ]),
        &["server", "app"]
    )
    .unwrap()
});

/// Active connections per app.
pub static HTTP_ACTIVE_CONNECTIONS: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    register_int_gauge_vec!(
        Opts::new(
            "tako_http_active_connections",
            "Currently active HTTP connections per app"
        ),
        &["server", "app"]
    )
    .unwrap()
});

/// Cold starts triggered per app.
pub static COLD_STARTS_TOTAL: LazyLock<IntCounterVec> = LazyLock::new(|| {
    register_int_counter_vec!(
        Opts::new(
            "tako_cold_starts_total",
            "Total cold starts triggered per app"
        ),
        &["server", "app"]
    )
    .unwrap()
});

/// Cold start duration in seconds per app.
pub static COLD_START_DURATION_SECONDS: LazyLock<HistogramVec> = LazyLock::new(|| {
    register_histogram_vec!(
        HistogramOpts::new(
            "tako_cold_start_duration_seconds",
            "Cold start duration in seconds"
        )
        .buckets(vec![0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0]),
        &["server", "app"]
    )
    .unwrap()
});

/// Instance health status gauge (1 = healthy, 0 = unhealthy).
pub static INSTANCE_HEALTH: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    register_int_gauge_vec!(
        Opts::new(
            "tako_instance_health",
            "Instance health status (1=healthy, 0=unhealthy)"
        ),
        &["server", "app", "instance"]
    )
    .unwrap()
});

/// Running instance count per app.
pub static INSTANCES_RUNNING: LazyLock<IntGaugeVec> = LazyLock::new(|| {
    register_int_gauge_vec!(
        Opts::new(
            "tako_instances_running",
            "Number of running instances per app"
        ),
        &["server", "app"]
    )
    .unwrap()
});

/// Convenience helper for timing a request. Call `.finish()` in the logging phase.
pub struct RequestTimer {
    app: String,
    start: Instant,
}

impl RequestTimer {
    pub fn start(app: String) -> Self {
        HTTP_ACTIVE_CONNECTIONS
            .with_label_values(&[server(), &app])
            .inc();
        Self { app, start: Instant::now() }
    }

    pub fn finish(self, status: u16) {
        let duration = self.start.elapsed().as_secs_f64();
        let status_str = status_class(status);
        let srv = server();

        HTTP_REQUESTS_TOTAL
            .with_label_values(&[srv, &self.app, status_str])
            .inc();
        HTTP_REQUEST_DURATION_SECONDS
            .with_label_values(&[srv, &self.app])
            .observe(duration);
        HTTP_ACTIVE_CONNECTIONS
            .with_label_values(&[srv, &self.app])
            .dec();
    }
}

/// Map status code to a class string for the label.
fn status_class(status: u16) -> &'static str {
    match status {
        200..=299 => "2xx",
        300..=399 => "3xx",
        400..=499 => "4xx",
        500..=599 => "5xx",
        _ => "other",
    }
}

/// Record a cold start event.
pub fn record_cold_start(app: &str, duration_secs: f64) {
    let srv = server();
    COLD_STARTS_TOTAL.with_label_values(&[srv, app]).inc();
    COLD_START_DURATION_SECONDS
        .with_label_values(&[srv, app])
        .observe(duration_secs);
}

/// Update the health gauge for an instance.
pub fn set_instance_health(app: &str, instance_id: &str, healthy: bool) {
    INSTANCE_HEALTH
        .with_label_values(&[server(), app, instance_id])
        .set(if healthy { 1 } else { 0 });
}

/// Remove all metric series for an instance (when it's removed).
pub fn remove_instance_metrics(app: &str, instance_id: &str) {
    let _ = INSTANCE_HEALTH.remove_label_values(&[server(), app, instance_id]);
}

/// Set the running instance count for an app.
pub fn set_instances_running(app: &str, count: i64) {
    INSTANCES_RUNNING
        .with_label_values(&[server(), app])
        .set(count);
}

/// Initialize metrics with the server identity. Call once at startup.
/// Uses the provided server name (from config file), falling back to hostname.
pub fn init(server_name: Option<&str>) {
    let label = server_name
        .map(String::from)
        .or_else(|| hostname::get().ok().and_then(|h| h.into_string().ok()))
        .unwrap_or_else(|| "unknown".to_string());
    let _ = SERVER_LABEL.set(label);

    LazyLock::force(&HTTP_REQUESTS_TOTAL);
    LazyLock::force(&HTTP_REQUEST_DURATION_SECONDS);
    LazyLock::force(&HTTP_ACTIVE_CONNECTIONS);
    LazyLock::force(&COLD_STARTS_TOTAL);
    LazyLock::force(&COLD_START_DURATION_SECONDS);
    LazyLock::force(&INSTANCE_HEALTH);
    LazyLock::force(&INSTANCES_RUNNING);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_status_class() {
        assert_eq!(status_class(200), "2xx");
        assert_eq!(status_class(301), "3xx");
        assert_eq!(status_class(404), "4xx");
        assert_eq!(status_class(500), "5xx");
        assert_eq!(status_class(0), "other");
    }

    #[test]
    fn test_request_timer_records_metrics() {
        init(Some("test-server"));
        let timer = RequestTimer::start("test-app".to_string());
        timer.finish(200);

        let count = HTTP_REQUESTS_TOTAL
            .with_label_values(&[server(), "test-app", "2xx"])
            .get();
        assert!(count >= 1);
    }

    #[test]
    fn test_server_label_is_set() {
        init(Some("test-server"));
        assert!(!server().is_empty());
        assert_ne!(server(), "unknown");
    }
}
