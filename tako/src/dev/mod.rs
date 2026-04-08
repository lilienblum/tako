//! Tako Local Development Support
//!
//! This module provides functionality for local development with Tako:
//!
//! - **Local CA**: Generates trusted HTTPS certificates for `*.tako` domains
//! - **Domain**: Shared domain constants and utilities

mod ca;
mod domain;

pub use ca::{CaError, Certificate, LocalCA, LocalCAStore};
pub use domain::{SHORT_DEV_DOMAIN, TAKO_DEV_DOMAIN, get_short_domain, get_tako_domain};
