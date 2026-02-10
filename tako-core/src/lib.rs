//! Tako Core - Shared protocol types
//!
//! This crate contains the protocol types shared between the Tako CLI (`tako`)
//! and the Tako server (`tako-server`) for communication via Unix sockets.
//!
//! All CLI-specific functionality (SSH, build, runtime detection, local dev CA, etc.)
//! lives in the `tako` crate.

mod protocol;

pub use protocol::*;
