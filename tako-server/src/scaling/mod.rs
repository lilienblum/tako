//! On-demand scaling
//!
//! Handles:
//! - Cold start: Starting instances when requests arrive for idle apps
//! - Idle timeout: Stopping instances after period of inactivity

mod cold_start;
mod idle;

#[allow(unused_imports)]
pub use cold_start::*;
#[allow(unused_imports)]
pub use idle::*;
