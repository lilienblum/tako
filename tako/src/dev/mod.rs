//! Tako Local Development Support
//!
//! This module provides functionality for local development with Tako:
//!
//! - **Local CA**: Generates trusted HTTPS certificates for `*.tako.local` domains
//! - **Domain**: Shared domain constants and utilities

mod ca;
mod domain;

pub use ca::{CaError, Certificate, LocalCA, LocalCAStore};
pub use domain::{TAKO_LOCAL_DOMAIN, get_tako_domain};
