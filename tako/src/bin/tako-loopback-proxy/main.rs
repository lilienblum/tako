#[cfg(target_os = "macos")]
mod macos {
    use std::ffi::CString;
    use std::os::fd::{FromRawFd, OwnedFd};
    use std::process::Command;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use tokio::net::{TcpListener, TcpStream};

    const HTTP_SOCKET_NAME: &str = "http";
    const HTTPS_SOCKET_NAME: &str = "https";
    const HTTP_UPSTREAM: &str = "127.0.0.1:47830";
    const HTTPS_UPSTREAM: &str = "127.0.0.1:47831";
    const LOOPBACK_ADDR: &str = "127.77.0.1";
    const LOOPBACK_INTERFACE: &str = "lo0";
    const LOOPBACK_PROXY_LABEL: &str = "sh.tako.loopback-proxy";
    const LOOPBACK_PROXY_PLIST_PATH: &str =
        "/Library/Application Support/Tako/launchd/sh.tako.loopback-proxy.plist";
    const IDLE_TIMEOUT: Duration = Duration::from_secs(4 * 60 * 60);
    const IDLE_TICK: Duration = Duration::from_secs(60);
    const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

    unsafe extern "C" {
        fn launch_activate_socket(
            name: *const libc::c_char,
            fds: *mut *mut libc::c_int,
            cnt: *mut libc::size_t,
        ) -> libc::c_int;
    }

    #[derive(Clone)]
    struct ProxyState {
        active_connections: Arc<AtomicUsize>,
        // This lock is only used for a best-effort idle timestamp and is never held
        // across an await point, so a synchronous mutex keeps the state simple here.
        last_activity: Arc<std::sync::Mutex<Instant>>,
    }

    impl ProxyState {
        fn new() -> Self {
            Self {
                active_connections: Arc::new(AtomicUsize::new(0)),
                last_activity: Arc::new(std::sync::Mutex::new(Instant::now())),
            }
        }

        fn connection_started(&self) -> ActiveConnectionGuard {
            self.active_connections.fetch_add(1, Ordering::Relaxed);
            self.record_activity();
            ActiveConnectionGuard {
                state: self.clone(),
            }
        }

        fn record_activity(&self) {
            if let Ok(mut last_activity) = self.last_activity.lock() {
                *last_activity = Instant::now();
            }
        }

        fn should_exit_for_idle(&self) -> bool {
            let idle_for = self
                .last_activity
                .lock()
                .map(|instant| instant.elapsed())
                .unwrap_or_default();
            self.active_connections.load(Ordering::Relaxed) == 0 && idle_for >= IDLE_TIMEOUT
        }
    }

    struct ActiveConnectionGuard {
        state: ProxyState,
    }

    impl Drop for ActiveConnectionGuard {
        fn drop(&mut self) {
            self.state
                .active_connections
                .fetch_sub(1, Ordering::Relaxed);
            self.state.record_activity();
        }
    }

    pub(crate) async fn run() -> Result<(), Box<dyn std::error::Error>> {
        let state = ProxyState::new();
        let mut listeners = activated_listeners(HTTPS_SOCKET_NAME)?
            .into_iter()
            .map(|listener| (listener, HTTPS_UPSTREAM))
            .collect::<Vec<_>>();
        listeners.extend(
            activated_listeners(HTTP_SOCKET_NAME)?
                .into_iter()
                .map(|listener| (listener, HTTP_UPSTREAM)),
        );

        let (error_tx, mut error_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        for (listener, upstream) in listeners {
            let state = state.clone();
            let error_tx = error_tx.clone();
            tokio::spawn(async move {
                if let Err(error) = run_listener(listener, upstream, state).await {
                    let _ = error_tx.send(error.to_string());
                }
            });
        }
        drop(error_tx);

        let mut idle_tick = tokio::time::interval(IDLE_TICK);
        loop {
            tokio::select! {
                // Treat either launchd socket as required local ingress. If one listener
                // fails, restart the whole helper into a clean state instead of running
                // half-alive with only HTTP or HTTPS working.
                Some(error) = error_rx.recv() => return Err(error.into()),
                _ = idle_tick.tick() => {
                    if state.should_exit_for_idle() {
                        return Ok(());
                    }
                }
            }
        }
    }

    pub(crate) fn bootstrap() -> Result<(), Box<dyn std::error::Error>> {
        ensure_running_as_root(unsafe { libc::geteuid() })?;
        ensure_loopback_alias()?;
        rebootstrap_proxy_launchd_job()?;
        Ok(())
    }

    async fn run_listener(
        listener: TcpListener,
        upstream: &'static str,
        state: ProxyState,
    ) -> Result<(), std::io::Error> {
        loop {
            let (stream, _) = listener.accept().await?;
            state.record_activity();
            let state = state.clone();
            tokio::spawn(async move {
                proxy_connection(stream, upstream, state).await;
            });
        }
    }

    async fn proxy_connection(stream: TcpStream, upstream: &'static str, state: ProxyState) {
        let _guard = state.connection_started();
        let Ok(Ok(mut upstream_stream)) =
            tokio::time::timeout(UPSTREAM_CONNECT_TIMEOUT, TcpStream::connect(upstream)).await
        else {
            return;
        };
        let mut downstream = stream;
        let _ = tokio::io::copy_bidirectional(&mut downstream, &mut upstream_stream).await;
    }

    fn activated_listeners(name: &str) -> Result<Vec<TcpListener>, Box<dyn std::error::Error>> {
        let name = CString::new(name)?;
        let mut fds = std::ptr::null_mut();
        let mut count = 0usize;
        let rc = unsafe { launch_activate_socket(name.as_ptr(), &mut fds, &mut count) };
        if rc != 0 {
            return Err(std::io::Error::from_raw_os_error(rc).into());
        }
        if fds.is_null() || count == 0 {
            return Err(format!("launchd did not provide any sockets for {}", name.to_string_lossy()).into());
        }

        let raw_fds = take_activated_socket_fds(fds, count);
        let mut listeners = Vec::with_capacity(raw_fds.len());
        for fd in raw_fds {
            let owned = unsafe { OwnedFd::from_raw_fd(fd) };
            let std_listener = std::net::TcpListener::from(owned);
            std_listener.set_nonblocking(true)?;
            listeners.push(TcpListener::from_std(std_listener)?);
        }
        Ok(listeners)
    }

    fn take_activated_socket_fds(fds: *mut libc::c_int, count: usize) -> Vec<libc::c_int> {
        unsafe {
            let slice = std::slice::from_raw_parts(fds, count);
            let raw_fds = slice.to_vec();
            libc::free(fds.cast());
            raw_fds
        }
    }

    fn ensure_loopback_alias() -> Result<(), Box<dyn std::error::Error>> {
        if loopback_alias_ready()? {
            return Ok(());
        }

        run_checked(
            Command::new("ifconfig").args(["lo0", "alias", LOOPBACK_ADDR, "up"]),
            "assigning Tako loopback alias",
        )
    }

    fn loopback_alias_ready() -> Result<bool, Box<dyn std::error::Error>> {
        let output = Command::new("ifconfig").arg(LOOPBACK_INTERFACE).output()?;
        if !output.status.success() {
            return Ok(false);
        }
        Ok(loopback_alias_present(
            &String::from_utf8_lossy(&output.stdout),
            LOOPBACK_ADDR,
        ))
    }

    fn loopback_alias_present(ifconfig_output: &str, ip: &str) -> bool {
        ifconfig_output.lines().any(|line| {
            let mut parts = line.split_whitespace();
            matches!(parts.next(), Some("inet")) && parts.next() == Some(ip)
        })
    }

    fn ensure_running_as_root(euid: u32) -> Result<(), Box<dyn std::error::Error>> {
        if euid == 0 {
            return Ok(());
        }
        Err("tako-loopback-proxy bootstrap must run as root".into())
    }

    fn rebootstrap_proxy_launchd_job() -> Result<(), Box<dyn std::error::Error>> {
        let label = format!("system/{LOOPBACK_PROXY_LABEL}");
        let bootout = Command::new("launchctl").args(["bootout", &label]).status()?;
        if !(bootout.success() || bootout.code() == Some(3)) {
            return Err("booting out loopback proxy launchd service failed".into());
        }
        run_checked(
            Command::new("launchctl").args(["bootstrap", "system", LOOPBACK_PROXY_PLIST_PATH]),
            "bootstrapping loopback proxy launchd service",
        )?;
        run_checked(
            Command::new("launchctl").args(["enable", &label]),
            "enabling loopback proxy launchd service",
        )
    }

    fn run_checked(
        command: &mut Command,
        context: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let output = command.output()?;
        if output.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() {
            stderr
        } else if !stdout.is_empty() {
            stdout
        } else {
            format!("exit status {}", output.status)
        };
        Err(format!("{context} failed: {detail}").into())
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn socket_names_map_to_expected_upstreams() {
            assert_eq!(HTTP_SOCKET_NAME, "http");
            assert_eq!(HTTPS_SOCKET_NAME, "https");
            assert_eq!(HTTP_UPSTREAM, "127.0.0.1:47830");
            assert_eq!(HTTPS_UPSTREAM, "127.0.0.1:47831");
        }

        #[test]
        fn idle_exit_requires_zero_connections_and_elapsed_timeout() {
            let state = ProxyState::new();
            assert!(!state.should_exit_for_idle());
            *state.last_activity.lock().expect("lock") =
                Instant::now() - IDLE_TIMEOUT - Duration::from_secs(1);
            assert!(state.should_exit_for_idle());
            state.active_connections.store(1, Ordering::Relaxed);
            assert!(!state.should_exit_for_idle());
        }

        #[test]
        fn loopback_alias_present_matches_assigned_ipv4_lines() {
            assert!(loopback_alias_present(
                "lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST>\n\tinet 127.0.0.1 netmask 0xff000000\n\tinet 127.77.0.1 netmask 0xff000000 alias\n",
                "127.77.0.1",
            ));
            assert!(!loopback_alias_present(
                "lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST>\n\tinet 127.0.0.1 netmask 0xff000000\n",
                "127.77.0.1",
            ));
        }

        #[test]
        fn run_checked_failure_includes_stderr() {
            let err = run_checked(
                Command::new("sh").args(["-c", "echo boom >&2; exit 7"]),
                "demo command",
            )
            .expect_err("expected failing command");

            let text = err.to_string();
            assert!(text.contains("demo command failed"));
            assert!(text.contains("boom"));
        }

        #[test]
        fn ensure_running_as_root_reports_clear_error_for_non_root() {
            let err = ensure_running_as_root(501).expect_err("non-root should fail");
            assert!(err.to_string().contains("must run as root"));
        }

        #[test]
        fn ensure_running_as_root_accepts_root() {
            ensure_running_as_root(0).expect("root should succeed");
        }
    }
}

#[cfg(target_os = "macos")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if matches!(std::env::args().nth(1).as_deref(), Some("bootstrap")) {
        macos::bootstrap()
    } else {
        macos::run().await
    }
}

#[cfg(not(target_os = "macos"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    Err("tako-loopback-proxy is only supported on macOS".into())
}
