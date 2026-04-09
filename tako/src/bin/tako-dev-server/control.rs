use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncReadExt, BufReader};
use tokio::net::UnixStream;
use tokio::sync::mpsc;
use tokio::sync::watch;

use crate::process::{
    broadcast_app_status, kill_app_process, monitor_handoff_pid, push_app_event, push_divider,
    push_scoped_log, spawn_and_monitor_app,
};
use crate::protocol::{self, AppInfo, Request, Response};
use crate::state;
use crate::state::RuntimeApp;
use crate::{advertised_https_port, app_short_host, default_hosts};
use tako_socket::{read_json_line, write_json_line};

pub(crate) fn split_route_pattern(route: &str) -> (&str, Option<&str>) {
    match route.find('/') {
        Some(idx) => (&route[..idx], Some(&route[idx..])),
        None => (route, None),
    }
}

fn sanitize_app_name(name: &str) -> String {
    let mut out = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if (c == '_' || c == '.' || c == '-') && !out.ends_with('-') {
            out.push('-');
        }
    }
    while out.starts_with('-') || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.remove(0);
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "app".to_string()
    } else {
        out
    }
}

#[derive(Clone, Default)]
pub(crate) struct EventsHub {
    subs: Arc<Mutex<Vec<mpsc::UnboundedSender<Response>>>>,
}

impl EventsHub {
    pub(crate) fn subscribe(&self) -> mpsc::UnboundedReceiver<Response> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.subs.lock().unwrap().push(tx);
        rx
    }

    pub(crate) fn broadcast(&self, r: Response) {
        let mut subs = self.subs.lock().unwrap();
        subs.retain(|tx| tx.send(r.clone()).is_ok());
    }
}

pub(crate) struct State {
    pub(crate) events: EventsHub,

    pub(crate) shutdown_tx: watch::Sender<bool>,
    idle_generation: Arc<std::sync::atomic::AtomicU64>,

    pub(crate) routes: crate::proxy::Routes,
    pub(crate) local_dns_enabled: bool,
    pub(crate) local_dns_port: u16,

    pub(crate) listen_port: u16,
    pub(crate) listen_addr: String,
    pub(crate) advertised_ip: String,
    pub(crate) control_clients: u32,

    pub(crate) db: Option<state::DevStateStore>,
    pub(crate) apps: std::collections::HashMap<String, RuntimeApp>,
}

impl State {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        shutdown_tx: watch::Sender<bool>,
        routes: crate::proxy::Routes,
        events: EventsHub,
        local_dns_enabled: bool,
        local_dns_port: u16,
        listen_port: u16,
        listen_addr: String,
        advertised_ip: String,
    ) -> Self {
        Self {
            events,
            shutdown_tx,
            idle_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            routes,
            local_dns_enabled,
            local_dns_port,
            listen_port,
            listen_addr,
            advertised_ip,
            control_clients: 0,
            db: None,
            apps: std::collections::HashMap::new(),
        }
    }

    fn cancel_idle_exit(&mut self) {
        let _ = self
            .idle_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }

    fn schedule_idle_exit(&mut self) {
        let generation = self
            .idle_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        let shutdown_tx = self.shutdown_tx.clone();
        let idle_generation = self.idle_generation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(2)).await;
            if idle_generation.load(std::sync::atomic::Ordering::SeqCst) == generation {
                let _ = shutdown_tx.send(true);
            }
        });
    }
}

struct ControlClientSubscription {
    state: Arc<Mutex<State>>,
}

impl ControlClientSubscription {
    fn register(state: &Arc<Mutex<State>>) -> Self {
        if let Ok(mut s) = state.lock() {
            s.control_clients = s.control_clients.saturating_add(1);
        }
        Self {
            state: state.clone(),
        }
    }
}

impl Drop for ControlClientSubscription {
    fn drop(&mut self) {
        if let Ok(mut s) = self.state.lock() {
            s.control_clients = s.control_clients.saturating_sub(1);
        }
    }
}

pub(crate) async fn handle_client(
    stream: UnixStream,
    state: Arc<Mutex<State>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (r, mut w) = stream.into_split();
    let mut r = BufReader::new(r);

    loop {
        let Some(req) = (match read_json_line::<_, Request>(&mut r).await {
            Ok(v) => v,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                write_resp(
                    &mut w,
                    &Response::Error {
                        message: format!("invalid request: {}", e),
                    },
                )
                .await?;
                continue;
            }
            Err(e) => return Err(e.into()),
        }) else {
            break;
        };

        let resp = match req {
            Request::Ping => Response::Pong,
            Request::SubscribeEvents => {
                let rx = {
                    let s = state.lock().unwrap();
                    s.events.subscribe()
                };

                let _control_client = ControlClientSubscription::register(&state);
                let mut rx = rx;
                if write_resp(&mut w, &Response::Subscribed).await.is_err() {
                    return Ok(());
                }
                let mut disconnect_probe = [0_u8; 1];
                loop {
                    tokio::select! {
                        maybe_resp = rx.recv() => {
                            let Some(resp) = maybe_resp else {
                                break;
                            };
                            if write_resp(&mut w, &resp).await.is_err() {
                                break;
                            }
                        }
                        read_result = r.read(&mut disconnect_probe) => {
                            match read_result {
                                Ok(0) | Err(_) => break,
                                Ok(_) => {}
                            }
                        }
                    }
                }
                return Ok(());
            }
            Request::SubscribeLogs { config_path, after } => {
                let log_buffer = {
                    let s = state.lock().unwrap();
                    s.apps.get(&config_path).map(|a| a.log_buffer.clone())
                };

                let Some(log_buffer) = log_buffer else {
                    write_resp(
                        &mut w,
                        &Response::Error {
                            message: format!("app not found: {config_path}"),
                        },
                    )
                    .await?;
                    continue;
                };

                let _control_client = ControlClientSubscription::register(&state);
                let (backlog, mut rx, truncated) = log_buffer.subscribe(after);

                if write_resp(&mut w, &Response::LogsSubscribed).await.is_err() {
                    return Ok(());
                }
                if truncated && write_resp(&mut w, &Response::LogsTruncated).await.is_err() {
                    return Ok(());
                }

                for entry in backlog {
                    if write_resp(
                        &mut w,
                        &Response::LogEntry {
                            id: entry.id,
                            line: entry.line,
                        },
                    )
                    .await
                    .is_err()
                    {
                        return Ok(());
                    }
                }

                let mut disconnect_probe = [0_u8; 1];
                loop {
                    tokio::select! {
                        maybe_entry = rx.recv() => {
                            let Some(entry) = maybe_entry else {
                                break;
                            };
                            if write_resp(
                                &mut w,
                                &Response::LogEntry {
                                    id: entry.id,
                                    line: entry.line,
                                },
                            )
                            .await
                            .is_err()
                            {
                                break;
                            }
                        }
                        read_result = r.read(&mut disconnect_probe) => {
                            match read_result {
                                Ok(0) | Err(_) => break,
                                Ok(_) => {}
                            }
                        }
                    }
                }
                return Ok(());
            }
            Request::RegisterApp {
                config_path,
                project_dir,
                app_name,
                variant,
                hosts,
                upstream_port,
                command,
                env,
                client_pid,
            } => {
                let app_name = sanitize_app_name(&app_name);

                {
                    let s = state.lock().unwrap();
                    if let Some(existing) = s.apps.get(&config_path)
                        && let Some(pid) = existing.pid
                    {
                        kill_app_process(pid);
                    }
                }

                let mut s = state.lock().unwrap();
                s.cancel_idle_exit();

                let hosts = if hosts.is_empty() {
                    default_hosts(&app_name)
                } else {
                    hosts
                };

                if let Some(db) = &s.db {
                    let _ = db.register(&config_path, &project_dir, &app_name, variant.as_deref());
                }

                let log_buffer = s
                    .apps
                    .get(&config_path)
                    .map(|a| {
                        a.log_buffer.clear();
                        a.log_buffer.clone()
                    })
                    .unwrap_or_else(state::LogBuffer::new);

                s.apps.insert(
                    config_path.clone(),
                    RuntimeApp {
                        project_dir: project_dir.clone(),
                        name: app_name.clone(),
                        variant: variant.clone(),
                        hosts: hosts.clone(),
                        upstream_port,
                        is_idle: false,
                        command,
                        env,
                        log_buffer,
                        pid: None,
                        client_pid,
                    },
                );

                let route_id = format!("reg:{}", config_path);
                s.routes
                    .set_routes(route_id, hosts.clone(), upstream_port, true);

                let host = hosts
                    .first()
                    .cloned()
                    .unwrap_or_else(|| app_short_host(&app_name));
                let public_port = advertised_https_port(&s);
                let url = if public_port == 443 {
                    format!("https://{}/", host)
                } else {
                    format!("https://{}:{}/", host, public_port)
                };
                drop(s);

                let spawn_state = state.clone();
                let spawn_config = config_path.clone();
                tokio::spawn(async move {
                    match spawn_and_monitor_app(spawn_state.clone(), &spawn_config).await {
                        Ok(pid) => {
                            tracing::info!(config_path = %spawn_config, pid = pid, "spawned app process");
                            broadcast_app_status(&spawn_state, &spawn_config, "running");
                        }
                        Err(e) => {
                            tracing::warn!(config_path = %spawn_config, error = %e, "failed to spawn app");
                            let log_buffer = {
                                let s = spawn_state.lock().unwrap();
                                s.apps.get(&spawn_config).map(|a| a.log_buffer.clone())
                            };
                            broadcast_app_status(&spawn_state, &spawn_config, "idle");
                            if let Some(buf) = log_buffer {
                                let msg = format!("failed to start app: {e}");
                                push_scoped_log(&buf, "Error", "tako", &msg);
                                push_app_event(
                                    &buf,
                                    "error",
                                    Some(("message", serde_json::json!(msg))),
                                );
                            }
                        }
                    }
                });

                Response::AppRegistered {
                    app_name,
                    config_path,
                    project_dir,
                    url,
                }
            }
            Request::UnregisterApp { config_path } => {
                let mut s = state.lock().unwrap();

                if let Some(app) = s.apps.get(&config_path)
                    && let Some(pid) = app.pid
                {
                    kill_app_process(pid);
                    state::remove_pid_file(&app.project_dir, &config_path);
                }

                let app_name = s
                    .apps
                    .remove(&config_path)
                    .map(|a| a.name)
                    .unwrap_or_default();

                let route_id = format!("reg:{}", config_path);
                s.routes.remove_app(&route_id);

                if !app_name.is_empty() {
                    s.events.broadcast(Response::Event {
                        event: protocol::DevEvent::AppStatusChanged {
                            config_path: config_path.clone(),
                            app_name: app_name.clone(),
                            status: "stopped".to_string(),
                        },
                    });
                }

                if s.apps.is_empty() {
                    s.schedule_idle_exit();
                }

                Response::AppUnregistered { config_path }
            }
            Request::RestartApp { config_path } => {
                {
                    let mut s = state.lock().unwrap();
                    if let Some(app) = s.apps.get_mut(&config_path) {
                        if let Some(pid) = app.pid.take() {
                            kill_app_process(pid);
                            state::remove_pid_file(&app.project_dir, &config_path);
                        }
                        app.is_idle = true;
                    }
                }

                let log_buffer = {
                    let s = state.lock().unwrap();
                    s.apps.get(&config_path).map(|a| a.log_buffer.clone())
                };
                if let Some(ref buf) = log_buffer {
                    push_divider(buf, "restarted");
                }

                let spawn_state = state.clone();
                let spawn_config = config_path.clone();
                tokio::spawn(async move {
                    match spawn_and_monitor_app(spawn_state.clone(), &spawn_config).await {
                        Ok(pid) => {
                            tracing::info!(config_path = %spawn_config, pid = pid, "restarted app process");
                            broadcast_app_status(&spawn_state, &spawn_config, "running");
                        }
                        Err(e) => {
                            tracing::warn!(config_path = %spawn_config, error = %e, "failed to restart app");
                            let log_buffer = {
                                let s = spawn_state.lock().unwrap();
                                s.apps.get(&spawn_config).map(|a| a.log_buffer.clone())
                            };
                            if let Some(buf) = log_buffer {
                                let msg = format!("restart failed: {e}");
                                push_scoped_log(&buf, "Error", "tako", &msg);
                                push_app_event(
                                    &buf,
                                    "error",
                                    Some(("message", serde_json::json!(msg))),
                                );
                            }
                        }
                    }
                });

                Response::AppRestarting { config_path }
            }
            Request::SetAppStatus {
                config_path,
                status,
            } => {
                let is_idle = match status.as_str() {
                    "idle" => true,
                    "running" => false,
                    _ => {
                        write_resp(
                            &mut w,
                            &Response::Error {
                                message: format!("unknown status: {status}"),
                            },
                        )
                        .await?;
                        continue;
                    }
                };

                let mut s = state.lock().unwrap();
                let route_id = format!("reg:{}", config_path);
                s.routes.set_active(&route_id, !is_idle);

                let app_name = if let Some(app) = s.apps.get_mut(&config_path) {
                    app.is_idle = is_idle;
                    app.name.clone()
                } else {
                    String::new()
                };

                if !app_name.is_empty() {
                    s.events.broadcast(Response::Event {
                        event: protocol::DevEvent::AppStatusChanged {
                            config_path: config_path.clone(),
                            app_name,
                            status: status.clone(),
                        },
                    });
                }

                Response::AppStatusUpdated {
                    config_path,
                    status,
                }
            }
            Request::HandoffApp { config_path, pid } => {
                let mut s = state.lock().unwrap();
                let project_dir = if let Some(app) = s.apps.get_mut(&config_path) {
                    app.pid = Some(pid);
                    app.is_idle = false;
                    app.project_dir.clone()
                } else {
                    String::new()
                };
                if !project_dir.is_empty() {
                    state::write_pid_file(&project_dir, &config_path, pid);
                }

                let state_for_monitor = state.clone();
                let config_for_monitor = config_path.clone();
                let dir_for_monitor = project_dir.clone();
                tokio::spawn(async move {
                    monitor_handoff_pid(
                        state_for_monitor,
                        config_for_monitor,
                        dir_for_monitor,
                        pid,
                    )
                    .await;
                });

                Response::AppHandedOff { config_path }
            }
            Request::ConnectClient {
                config_path,
                client_id,
            } => {
                let app_name = {
                    let s = state.lock().unwrap();
                    let name = s
                        .apps
                        .get(&config_path)
                        .map(|a| a.name.clone())
                        .unwrap_or_default();
                    if let Some(app) = s.apps.get(&config_path) {
                        push_scoped_log(
                            &app.log_buffer,
                            "Debug",
                            "tako",
                            &format!("Client {} connected", client_id),
                        );
                    }
                    s.events.broadcast(Response::Event {
                        event: protocol::DevEvent::ClientConnected {
                            config_path: config_path.clone(),
                            app_name: name.clone(),
                            client_id,
                        },
                    });
                    name
                };

                if write_resp(&mut w, &Response::Pong).await.is_err() {
                    return Ok(());
                }

                let mut probe = [0_u8; 1];
                loop {
                    match r.read(&mut probe).await {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }
                }

                {
                    let s = state.lock().unwrap();
                    if let Some(app) = s.apps.get(&config_path) {
                        push_scoped_log(
                            &app.log_buffer,
                            "Debug",
                            "tako",
                            &format!("Client {} disconnected", client_id),
                        );
                    }
                    s.events.broadcast(Response::Event {
                        event: protocol::DevEvent::ClientDisconnected {
                            config_path,
                            app_name,
                            client_id,
                        },
                    });
                }

                return Ok(());
            }
            Request::ListRegisteredApps => {
                let s = state.lock().unwrap();
                let apps = s
                    .apps
                    .iter()
                    .map(|(config_path, a)| protocol::RegisteredAppInfo {
                        config_path: config_path.clone(),
                        project_dir: a.project_dir.clone(),
                        app_name: a.name.clone(),
                        variant: a.variant.clone(),
                        hosts: a.hosts.clone(),
                        upstream_port: a.upstream_port,
                        status: if a.is_idle { "idle" } else { "running" }.to_string(),
                        pid: a.pid,
                        client_pid: a.client_pid,
                    })
                    .collect();
                Response::RegisteredApps { apps }
            }
            Request::ListApps => {
                let s = state.lock().unwrap();
                let apps = s
                    .apps
                    .values()
                    .map(|a| AppInfo {
                        app_name: a.name.clone(),
                        variant: a.variant.clone(),
                        hosts: a.hosts.clone(),
                        upstream_port: a.upstream_port,
                        pid: a.pid,
                    })
                    .collect();
                Response::Apps { apps }
            }
            Request::Info => {
                let s = state.lock().unwrap();
                Response::Info {
                    info: protocol::DevInfo {
                        listen: s.listen_addr.clone(),
                        port: advertised_https_port(&s),
                        advertised_ip: s.advertised_ip.clone(),
                        local_dns_enabled: s.local_dns_enabled,
                        local_dns_port: s.local_dns_port,
                        control_clients: s.control_clients,
                    },
                }
            }
            Request::StopServer => {
                let s = state.lock().unwrap();
                let _ = s.shutdown_tx.send(true);
                Response::Stopping
            }
        };

        write_resp(&mut w, &resp).await?;
    }

    Ok(())
}

async fn write_resp(
    w: &mut tokio::net::unix::OwnedWriteHalf,
    resp: &Response,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    write_json_line(w, resp).await?;
    Ok(())
}
