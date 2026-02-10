//! SSH client for remote server operations
//!
//! Provides async SSH connectivity for:
//! - Command execution
//! - File upload/download via SFTP
//! - Streaming command output

mod client;
mod error;
mod sftp;

pub use client::*;
pub use error::*;
pub use sftp::*;
