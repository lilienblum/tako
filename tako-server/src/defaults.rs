use std::time::Duration;

pub const HEALTH_CHECK_INTERVAL: Duration = Duration::from_secs(1);
pub const HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(300);
pub const IDLE_CHECK_INTERVAL_DEBUG: Duration = Duration::from_secs(1);
pub const IDLE_CHECK_INTERVAL_RELEASE: Duration = Duration::from_secs(30);
