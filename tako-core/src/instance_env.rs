//! Environment contract for Tako-managed app processes.
//!
//! Every process spawned by Tako — whether by `tako-server` (production) or
//! `tako-dev-server` (development) — sees the same base environment. This
//! module is that contract, so the two spawners can't drift apart and leave
//! the SDK with half its wiring (e.g. `TAKO_APP_NAME` set but
//! `TAKO_INTERNAL_SOCKET` missing, which produces confusing runtime-only
//! failures the first time workflow `.enqueue()` is called).

use std::collections::HashMap;
use std::path::Path;

/// Env var the SDK reads to locate the shared Tako internal unix socket.
pub const TAKO_INTERNAL_SOCKET_ENV: &str = "TAKO_INTERNAL_SOCKET";

/// Env var the SDK reads to tag every RPC with its owning app.
pub const TAKO_APP_NAME_ENV: &str = "TAKO_APP_NAME";

/// Tells the SDK to bind to an OS-assigned port and report it back on the fd 4
/// readiness pipe. Both spawners set this to "0".
pub const PORT_ENV: &str = "PORT";

/// Loopback-only bind; the proxy reaches the instance over 127.0.0.1.
pub const HOST_ENV: &str = "HOST";

/// The base env every Tako-managed app process inherits.
///
/// This is the union of what `tako-server` and `tako-dev-server` must set on
/// every spawn. Callers layer their own env (user vars, secrets, PATH tweaks)
/// on top; this method is about the Tako runtime contract itself.
pub struct TakoRuntimeEnv<'a> {
    /// Name the SDK tags every RPC with. Required — every Tako process
    /// belongs to exactly one app.
    pub app_name: &'a str,
    /// Path to the internal unix socket used by workflow `.enqueue()`,
    /// `workflowsEngine.signal()`, and server-side channel `.publish()`.
    ///
    /// `None` only in tests that don't need workflow/channel RPCs. Real
    /// spawns should always provide this — a missing socket at spawn time
    /// is the kind of silent misconfiguration the fail-early check in the
    /// SDK is designed to catch.
    pub internal_socket: Option<&'a Path>,
}

impl<'a> TakoRuntimeEnv<'a> {
    /// Insert the contract vars into `env`. Overwrites existing values so
    /// user-supplied env can't accidentally shadow Tako's own wiring.
    pub fn apply(&self, env: &mut HashMap<String, String>) {
        env.insert(PORT_ENV.to_string(), "0".to_string());
        env.insert(HOST_ENV.to_string(), "127.0.0.1".to_string());
        env.insert(TAKO_APP_NAME_ENV.to_string(), self.app_name.to_string());
        if let Some(sock) = self.internal_socket {
            env.insert(
                TAKO_INTERNAL_SOCKET_ENV.to_string(),
                sock.to_string_lossy().to_string(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_sets_port_and_host() {
        let mut env = HashMap::new();
        TakoRuntimeEnv {
            app_name: "demo",
            internal_socket: None,
        }
        .apply(&mut env);
        assert_eq!(env.get(PORT_ENV).map(String::as_str), Some("0"));
        assert_eq!(env.get(HOST_ENV).map(String::as_str), Some("127.0.0.1"));
    }

    #[test]
    fn apply_sets_app_name_always() {
        let mut env = HashMap::new();
        TakoRuntimeEnv {
            app_name: "demo",
            internal_socket: None,
        }
        .apply(&mut env);
        assert_eq!(env.get(TAKO_APP_NAME_ENV).map(String::as_str), Some("demo"));
    }

    #[test]
    fn apply_sets_internal_socket_when_provided() {
        let mut env = HashMap::new();
        let path = Path::new("/tmp/tako.sock");
        TakoRuntimeEnv {
            app_name: "demo",
            internal_socket: Some(path),
        }
        .apply(&mut env);
        assert_eq!(
            env.get(TAKO_INTERNAL_SOCKET_ENV).map(String::as_str),
            Some("/tmp/tako.sock"),
        );
    }

    #[test]
    fn apply_omits_internal_socket_when_none() {
        let mut env = HashMap::new();
        TakoRuntimeEnv {
            app_name: "demo",
            internal_socket: None,
        }
        .apply(&mut env);
        assert!(!env.contains_key(TAKO_INTERNAL_SOCKET_ENV));
    }

    #[test]
    fn apply_overrides_user_supplied_host() {
        // User env vars must not be able to shadow Tako's wiring. Proxy
        // routes to 127.0.0.1, so a stray `HOST=0.0.0.0` from user config
        // would silently make the app unreachable.
        let mut env = HashMap::new();
        env.insert(HOST_ENV.to_string(), "0.0.0.0".to_string());
        TakoRuntimeEnv {
            app_name: "demo",
            internal_socket: None,
        }
        .apply(&mut env);
        assert_eq!(env.get(HOST_ENV).map(String::as_str), Some("127.0.0.1"));
    }

    #[test]
    fn apply_overrides_user_supplied_app_name_and_socket() {
        let mut env = HashMap::new();
        env.insert(TAKO_APP_NAME_ENV.to_string(), "impostor".to_string());
        env.insert(
            TAKO_INTERNAL_SOCKET_ENV.to_string(),
            "/tmp/wrong.sock".to_string(),
        );
        TakoRuntimeEnv {
            app_name: "demo",
            internal_socket: Some(Path::new("/tmp/tako.sock")),
        }
        .apply(&mut env);
        assert_eq!(env.get(TAKO_APP_NAME_ENV).map(String::as_str), Some("demo"));
        assert_eq!(
            env.get(TAKO_INTERNAL_SOCKET_ENV).map(String::as_str),
            Some("/tmp/tako.sock"),
        );
    }
}
