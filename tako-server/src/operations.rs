use crate::app_command::env_vars_from_release_dir;
use crate::instances::{
    App, AppConfig, RollingUpdateConfig, RollingUpdater, target_new_instances_for_build,
};
use crate::metrics;
use crate::release::{
    app_root, apply_release_runtime_to_config, collect_running_build_statuses,
    current_release_version, directory_modified_unix_secs, ensure_app_runtime_data_dirs,
    inject_app_data_dir_env, prepare_release_runtime, read_release_manifest_metadata,
    requested_deployment_identity, resolve_release_runtime_bin, should_use_self_signed_route_cert,
    validate_app_name, validate_deploy_routes, validate_release_path_for_app,
    validate_release_version,
};
use crate::socket::{AppState, AppStatus, Command, InstanceState, InstanceStatus, Response};
use crate::tls::CertInfo;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tako_core::{HelloResponse, ListReleasesResponse, PROTOCOL_VERSION, ReleaseInfo};

impl super::ServerState {
    /// Handle a command from the management socket
    pub async fn handle_command(&self, cmd: Command) -> Response {
        match cmd {
            Command::Hello { protocol_version } => {
                let data = HelloResponse {
                    protocol_version: PROTOCOL_VERSION,
                    server_version: super::server_version().to_string(),
                    capabilities: vec![
                        "on_demand_cold_start".to_string(),
                        "idle_scale_to_zero".to_string(),
                        "scale".to_string(),
                        "upgrade_mode_control".to_string(),
                        "server_runtime_info".to_string(),
                        "release_history".to_string(),
                        "rollback".to_string(),
                    ],
                };

                if protocol_version != PROTOCOL_VERSION {
                    return Response::error(format!(
                        "Protocol version mismatch: client={} server={}",
                        protocol_version, PROTOCOL_VERSION
                    ));
                }

                Response::ok(data)
            }
            Command::PrepareRelease { app, path } => {
                if let Err(msg) = validate_app_name(&app) {
                    return Response::error(msg);
                }
                self.prepare_release(&app, &path).await
            }
            Command::Deploy {
                app,
                version,
                path,
                routes,
                secrets,
            } => {
                if let Err(msg) = validate_app_name(&app) {
                    return Response::error(msg);
                }
                if let Err(msg) = validate_release_version(&version) {
                    return Response::error(msg);
                }
                if let Some(resp) = self.reject_mutating_when_upgrading("deploy").await {
                    return resp;
                }
                self.deploy_app(&app, &version, &path, routes, secrets)
                    .await
            }
            Command::Scale { app, instances } => {
                if let Err(msg) = validate_app_name(&app) {
                    return Response::error(msg);
                }
                if let Some(resp) = self.reject_mutating_when_upgrading("scale").await {
                    return resp;
                }
                self.scale_app(&app, instances).await
            }
            Command::Stop { app } => {
                if let Err(msg) = validate_app_name(&app) {
                    return Response::error(msg);
                }
                if let Some(resp) = self.reject_mutating_when_upgrading("stop").await {
                    return resp;
                }
                self.stop_app(&app).await
            }
            Command::Delete { app } => {
                if let Err(msg) = validate_app_name(&app) {
                    return Response::error(msg);
                }
                if let Some(resp) = self.reject_mutating_when_upgrading("delete").await {
                    return resp;
                }
                self.delete_app(&app).await
            }
            Command::Status { app } => {
                if let Err(msg) = validate_app_name(&app) {
                    return Response::error(msg);
                }
                self.get_status(&app).await
            }
            Command::List => self.list_apps().await,
            Command::ListReleases { app } => {
                if let Err(msg) = validate_app_name(&app) {
                    return Response::error(msg);
                }
                self.list_releases(&app).await
            }
            Command::Routes => self.list_routes().await,
            Command::Rollback { app, version } => {
                if let Err(msg) = validate_app_name(&app) {
                    return Response::error(msg);
                }
                if let Err(msg) = validate_release_version(&version) {
                    return Response::error(msg);
                }
                if let Some(resp) = self.reject_mutating_when_upgrading("rollback").await {
                    return resp;
                }
                self.rollback_app(&app, &version).await
            }
            Command::UpdateSecrets { app, secrets } => {
                if let Err(msg) = validate_app_name(&app) {
                    return Response::error(msg);
                }
                if let Some(resp) = self.reject_mutating_when_upgrading("update-secrets").await {
                    return resp;
                }
                self.update_secrets(&app, secrets).await
            }
            Command::GetSecretsHash { app } => {
                if let Err(msg) = validate_app_name(&app) {
                    return Response::error(msg);
                }
                let secrets = self.state_store.get_secrets(&app).unwrap_or_default();
                let hash = tako_core::compute_secrets_hash(&secrets);
                Response::ok(serde_json::json!({ "hash": hash }))
            }
            Command::ServerInfo => Response::ok(self.runtime_info().await),
            Command::EnterUpgrading { owner } => match self.try_enter_upgrading(&owner).await {
                Ok(true) => Response::ok(serde_json::json!({
                    "status": "upgrading",
                    "owner": owner
                })),
                Ok(false) => {
                    let owner_msg = self
                        .state_store
                        .upgrade_lock_owner()
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| "unknown".to_string());
                    Response::error(format!(
                        "Server is already upgrading (owner: {}).",
                        owner_msg
                    ))
                }
                Err(e) => Response::error(format!("Failed to enter upgrading mode: {}", e)),
            },
            Command::ExitUpgrading { owner } => match self.exit_upgrading(&owner).await {
                Ok(true) => Response::ok(serde_json::json!({
                    "status": "normal",
                    "owner": owner
                })),
                Ok(false) => Response::error(
                    "Failed to exit upgrading mode: owner does not hold the upgrade lock."
                        .to_string(),
                ),
                Err(e) => Response::error(format!("Failed to exit upgrading mode: {}", e)),
            },
            Command::InjectChallengeToken {
                token,
                key_authorization,
            } => {
                let mut tokens = self.challenge_tokens.write();
                tokens.insert(token.clone(), key_authorization);
                Response::ok(serde_json::json!({
                    "status": "injected",
                    "token": token
                }))
            }
            Command::EnqueueRun { .. }
            | Command::RegisterSchedules { .. }
            | Command::ClaimRun { .. }
            | Command::HeartbeatRun { .. }
            | Command::SaveStep { .. }
            | Command::CompleteRun { .. }
            | Command::CancelRun { .. }
            | Command::FailRun { .. }
            | Command::DeferRun { .. }
            | Command::WaitForEvent { .. }
            | Command::Signal { .. }
            | Command::ChannelPublish { .. } => Response::error(
                "workflow/channel commands must be sent over the workflow socket, not the management socket"
                    .to_string(),
            ),
        }
    }

    async fn prepare_release(&self, app_name: &str, path: &str) -> Response {
        let release_path =
            match validate_release_path_for_app(&self.runtime.data_dir, app_name, path) {
                Ok(value) => value,
                Err(msg) => return Response::error(msg),
            };

        let env_vars = match env_vars_from_release_dir(&release_path) {
            Ok(vars) => vars,
            Err(error) => return Response::error(format!("Invalid app release: {}", error)),
        };

        let secrets = self.state_store.get_secrets(app_name).unwrap_or_default();
        let mut release_env = env_vars;
        release_env.extend(secrets);
        let data_paths = match ensure_app_runtime_data_dirs(&self.runtime.data_dir, app_name) {
            Ok(paths) => paths,
            Err(error) => return Response::error(format!("Release preparation failed: {error}")),
        };
        inject_app_data_dir_env(&mut release_env, &data_paths);

        match prepare_release_runtime(&release_path, &release_env, &self.runtime.data_dir).await {
            Ok(_) => Response::ok(serde_json::json!({ "status": "prepared" })),
            Err(error) => Response::error(format!("Release preparation failed: {error}")),
        }
    }

    async fn deploy_app(
        &self,
        app_name: &str,
        version: &str,
        path: &str,
        routes: Vec<String>,
        secrets: Option<HashMap<String, String>>,
    ) -> Response {
        tracing::info!(app = app_name, version = version, "Deploying app");

        if let Err(msg) = validate_app_name(app_name) {
            return Response::error(msg);
        }
        if let Err(msg) = validate_release_version(version) {
            return Response::error(msg);
        }
        if let Err(msg) = validate_deploy_routes(&routes) {
            return Response::error(msg);
        }
        let release_path =
            match validate_release_path_for_app(&self.runtime.data_dir, app_name, path) {
                Ok(value) => value,
                Err(msg) => return Response::error(msg),
            };

        let lock = self.get_deploy_lock(app_name).await;
        let _guard = match lock.try_lock() {
            Ok(guard) => guard,
            Err(_) => {
                tracing::warn!(
                    app = app_name,
                    "Deploy rejected: another deploy in progress"
                );
                return Response::error(format!(
                    "Deploy already in progress for app '{}'. Please wait and try again.",
                    app_name
                ));
            }
        };

        let env_vars = match env_vars_from_release_dir(&release_path) {
            Ok(vars) => vars,
            Err(error) => return Response::error(format!("Invalid app release: {}", error)),
        };
        let data_paths = match ensure_app_runtime_data_dirs(&self.runtime.data_dir, app_name) {
            Ok(paths) => paths,
            Err(error) => {
                return Response::error(format!("Failed to create app data dirs: {error}"));
            }
        };

        let secrets = if let Some(new_secrets) = secrets {
            if let Err(e) = self.state_store.set_secrets(app_name, &new_secrets) {
                return Response::error(format!("Failed to store secrets: {}", e));
            }
            new_secrets
        } else {
            self.state_store.get_secrets(app_name).unwrap_or_default()
        };
        let mut release_env = env_vars.clone();
        inject_app_data_dir_env(&mut release_env, &data_paths);
        release_env.extend(secrets.clone());

        let runtime_bin_path =
            match resolve_release_runtime_bin(&release_path, &self.runtime.data_dir).await {
                Ok(bin) => bin,
                Err(error) => return Response::error(format!("Invalid app release: {}", error)),
            };

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&release_path, std::fs::Permissions::from_mode(0o750));
        }

        let (app, deploy_config, is_new_app) =
            if let Some(existing) = self.app_manager.get_app(app_name) {
                let mut config = existing.config.read().clone();
                config.version = version.to_string();
                config.secrets = secrets;
                if let Err(error) = apply_release_runtime_to_config(
                    &mut config,
                    release_path.clone(),
                    runtime_bin_path.as_deref(),
                ) {
                    return Response::error(format!("Invalid app release: {}", error));
                }
                inject_app_data_dir_env(&mut config.env_vars, &data_paths);
                existing.update_config(config.clone());
                (existing, config, false)
            } else {
                let (name, environment) = requested_deployment_identity(app_name);
                let config = AppConfig {
                    name,
                    environment,
                    version: version.to_string(),
                    secrets,
                    min_instances: 0,
                    max_instances: 4,
                    ..Default::default()
                };
                let mut config = config;
                if let Err(error) = apply_release_runtime_to_config(
                    &mut config,
                    release_path.clone(),
                    runtime_bin_path.as_deref(),
                ) {
                    return Response::error(format!("Invalid app release: {}", error));
                }
                inject_app_data_dir_env(&mut config.env_vars, &data_paths);

                let deploy_config = config.clone();
                let app = self.app_manager.register_app(config);
                self.load_balancer.register_app(app.clone());
                (app, deploy_config, true)
            };

        {
            let mut route_table = self.routes.write().await;
            route_table.set_app_routes(app_name.to_string(), routes.clone());
        }

        app.clear_last_error();

        for route in &routes {
            let domain = route.split('/').next().unwrap_or(route);
            self.ensure_route_certificate(app_name, domain).await;
        }

        // Bring up the workflow engine for this app if it ships a `workflows/`
        // directory. Scale-to-zero by default — no worker process spawns until
        // the first enqueue or cron tick. Idempotent: a re-deploy of an app
        // already under management is a no-op here.
        self.ensure_app_workflows(app_name, &release_path, runtime_bin_path.as_deref())
            .await;

        if app.get_instances().is_empty() {
            if deploy_config.min_instances == 0 {
                match self.start_on_demand_warm_instance(&app).await {
                    Ok(()) => {
                        app.set_state(AppState::Running);
                        self.cold_start.reset(app_name);
                        self.persist_app_state(app_name).await;
                        Response::ok(serde_json::json!({
                            "status": "deployed",
                            "app": app_name,
                            "version": version,
                            "new_app": is_new_app,
                            "on_demand": true,
                            "startup_validated": true,
                            "warm_instance": true
                        }))
                    }
                    Err(e) => {
                        app.set_state(AppState::Error);
                        Response::error(format!("Deploy failed: {}", e))
                    }
                }
            } else {
                match self.app_manager.start_app(app_name).await {
                    Ok(()) => {
                        app.set_state(AppState::Running);
                        self.persist_app_state(app_name).await;
                        Response::ok(serde_json::json!({
                            "status": "deployed",
                            "app": app_name,
                            "version": version,
                            "new_app": is_new_app,
                            "on_demand": false
                        }))
                    }
                    Err(e) => {
                        app.set_state(AppState::Error);
                        Response::error(format!("Deploy failed: {}", e))
                    }
                }
            }
        } else {
            let previous_state = app.state();
            app.set_state(AppState::Deploying);

            let rolling_config = RollingUpdateConfig::default();
            let updater = RollingUpdater::new(self.app_manager.spawner().clone(), rolling_config);
            let target_new_instances = target_new_instances_for_build(
                deploy_config.min_instances,
                app.get_instances().len(),
            );

            match updater
                .update(&app, deploy_config.clone(), target_new_instances)
                .await
            {
                Ok(result) => {
                    if result.success {
                        if deploy_config.min_instances == 0 {
                            app.set_state(AppState::Running);
                            self.cold_start.reset(app_name);
                            self.persist_app_state(app_name).await;
                            Response::ok(serde_json::json!({
                                "status": "deployed",
                                "app": app_name,
                                "version": version,
                                "new_instances": result.new_instances,
                                "old_instances": result.old_instances,
                                "rolled_back": false,
                                "on_demand": true,
                                "startup_validated": true,
                                "warm_instance": true
                            }))
                        } else {
                            app.set_state(AppState::Running);
                            self.persist_app_state(app_name).await;
                            Response::ok(serde_json::json!({
                                "status": "deployed",
                                "app": app_name,
                                "version": version,
                                "new_instances": result.new_instances,
                                "old_instances": result.old_instances,
                                "rolled_back": false
                            }))
                        }
                    } else {
                        app.set_state(previous_state);
                        Response::error(
                            serde_json::json!({
                                "status": "rollback",
                                "app": app_name,
                                "error": result.error,
                                "rolled_back": true
                            })
                            .to_string(),
                        )
                    }
                }
                Err(e) => {
                    app.set_state(AppState::Error);
                    Response::error(format!("Rolling update failed: {}", e))
                }
            }
        }
    }

    /// Opt an app into the workflow engine when it ships a `workflows/`
    /// directory. Called at the end of a successful deploy.
    ///
    /// v1 defaults: `workers = 0` (scale-to-zero), `concurrency = 10`,
    /// `idle_timeout_ms = 300_000` (5 min). Per-server config from
    /// `[servers.X.workflows]` in `tako.toml` is *not yet* plumbed through
    /// the Deploy command — tracked as a follow-up.
    ///
    /// `runtime_bin_path` is the bun/node/deno binary resolved by
    /// `resolve_release_runtime_bin` — the same path the instance spawner
    /// uses, so workers share the pinned runtime version. Falls back to
    /// `bun` on `PATH` when unresolved (e.g. a source-build app in dev).
    async fn ensure_app_workflows(
        &self,
        app_name: &str,
        release_path: &std::path::Path,
        runtime_bin_path: Option<&str>,
    ) {
        let workflows_dir = release_path.join("workflows");
        if !workflows_dir.is_dir() {
            return;
        }

        let worker_entry = release_path
            .join("node_modules")
            .join("tako.sh")
            .join("dist")
            .join("tako")
            .join("entrypoints")
            .join("bun-worker.mjs");
        if !worker_entry.exists() {
            tracing::warn!(
                app = app_name,
                path = %worker_entry.display(),
                "Skipping workflow engine: worker entrypoint not found in release"
            );
            return;
        }

        let runtime_bin = runtime_bin_path
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| std::path::PathBuf::from("bun"));

        // Bring up the shared workflow socket the first time we ensure any
        // app. Idempotent — start_socket short-circuits if already running.
        if let Err(e) = self.workflows.start_socket() {
            tracing::warn!(error = %e, "Failed to start workflow socket");
            return;
        }
        let workflow_socket = self.workflows.socket_path();

        // Secrets are handed to the worker over fd 3 (same ABI as HTTP
        // instances). Fetch once before the closure so `ensure` owns them.
        let secrets = self.state_store.get_secrets(app_name).unwrap_or_default();

        let app = app_name.to_string();
        let app_for_spec = app.clone();
        let release = release_path.to_path_buf();
        let worker_bin = worker_entry;
        let manager = self.workflows.clone();
        let result = manager
            .ensure(&app, move |_db_path| {
                crate::workflows::worker_spec_for_bun(
                    &app_for_spec,
                    0,       // workers (scale-to-zero)
                    10,      // concurrency
                    300_000, // idle_timeout_ms (5 min)
                    &workflow_socket,
                    &runtime_bin,
                    &worker_bin,
                    &release,
                    secrets,
                )
            })
            .await;

        if let Err(e) = result {
            tracing::warn!(
                app = app_name,
                error = %e,
                "Failed to bring up workflow engine"
            );
        }
    }

    async fn stop_app(&self, app_name: &str) -> Response {
        tracing::info!(app = app_name, "Stopping app");

        // Drain workflow worker first so in-flight tasks get a chance to
        // finish before HTTP instances are torn down. 120s hard cap.
        self.workflows
            .stop(app_name, Duration::from_secs(120))
            .await;

        match self.app_manager.stop_app(app_name).await {
            Ok(()) => Response::ok(serde_json::json!({
                "status": "stopped",
                "app": app_name
            })),
            Err(e) => Response::error(format!("Stop failed: {}", e)),
        }
    }

    async fn scale_app(&self, app_name: &str, requested_instances: u8) -> Response {
        tracing::info!(app = app_name, requested_instances, "Scaling app");

        let app = match self.app_manager.get_app(app_name) {
            Some(app) => app,
            None => return Response::error(format!("App not found: {}", app_name)),
        };

        let previous_config = app.config.read().clone();
        let effective_instances = if self.runtime.standby {
            requested_instances.min(1)
        } else {
            requested_instances
        };

        let mut next_config = previous_config.clone();
        next_config.min_instances = effective_instances as u32;
        if next_config.max_instances < next_config.min_instances {
            next_config.max_instances = next_config.min_instances.max(4);
        }
        app.update_config(next_config.clone());

        let running_before = app
            .get_instances()
            .into_iter()
            .filter(|instance| {
                matches!(
                    instance.state(),
                    InstanceState::Starting | InstanceState::Ready | InstanceState::Healthy
                )
            })
            .count();

        if effective_instances as usize > running_before {
            let to_add = effective_instances as usize - running_before;
            let mut started_instances = Vec::with_capacity(to_add);

            for _ in 0..to_add {
                let instance = app.allocate_instance();
                match self
                    .app_manager
                    .spawner()
                    .spawn(&app, instance.clone())
                    .await
                {
                    Ok(()) => started_instances.push(instance),
                    Err(error) => {
                        for started in started_instances {
                            let _ = started.kill().await;
                            app.remove_instance(&started.id);
                        }
                        app.update_config(previous_config);
                        return Response::error(format!("Scale failed: {}", error));
                    }
                }
            }
        } else if (effective_instances as usize) < running_before {
            let mut candidates: Vec<_> = app
                .get_instances()
                .into_iter()
                .filter(|instance| {
                    matches!(
                        instance.state(),
                        InstanceState::Starting | InstanceState::Ready | InstanceState::Healthy
                    )
                })
                .collect();
            candidates.sort_by_key(|instance| std::cmp::Reverse(instance.idle_time()));

            let to_remove = running_before - effective_instances as usize;
            for instance in candidates.into_iter().take(to_remove) {
                if let Err(error) = self.drain_and_stop_instance(&app, &instance).await {
                    return Response::error(format!("Scale failed: {}", error));
                }
            }
        }

        crate::runtime_events::update_instance_count_metric(app_name, &app);
        if app.get_instances().is_empty() && effective_instances == 0 {
            app.set_state(AppState::Idle);
            self.cold_start.reset(app_name);
        } else {
            app.set_state(AppState::Running);
        }

        self.persist_app_state(app_name).await;

        Response::ok(serde_json::json!({
            "status": "scaled",
            "app": app_name,
            "instances": effective_instances,
            "requested_instances": requested_instances,
            "standby_limited": self.runtime.standby && effective_instances != requested_instances
        }))
    }

    async fn drain_and_stop_instance(
        &self,
        app: &Arc<App>,
        instance: &Arc<crate::instances::Instance>,
    ) -> Result<(), String> {
        instance.set_state(InstanceState::Draining);
        let deadline = tokio::time::Instant::now() + RollingUpdateConfig::default().drain_timeout;
        while instance.in_flight() > 0 {
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(
                    app = %app.name(),
                    instance = %instance.id,
                    in_flight = instance.in_flight(),
                    "Scale drain timeout exceeded, forcing stop"
                );
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }

        instance
            .kill()
            .await
            .map_err(|error| format!("failed to stop instance '{}': {}", instance.id, error))?;
        app.remove_instance(&instance.id);
        metrics::remove_instance_metrics(&app.name(), &instance.id);
        Ok(())
    }

    async fn delete_app(&self, app_name: &str) -> Response {
        tracing::info!(app = app_name, "Deleting app");

        // Drain workflow resources (worker, cron, enqueue socket) + remove
        // runs.db BEFORE we nuke app_root — the manager owns those files.
        self.workflows
            .delete(app_name, Duration::from_secs(120))
            .await;

        let mut existed = false;
        if self.app_manager.get_app(app_name).is_some() {
            existed = true;
            if let Err(e) = self.app_manager.stop_app(app_name).await {
                return Response::error(format!("Delete failed: {}", e));
            }
            self.app_manager.remove_app(app_name);
        }

        self.load_balancer.unregister_app(app_name);
        self.cold_start.reset(app_name);

        {
            let mut route_table = self.routes.write().await;
            route_table.remove_app_routes(app_name);
        }

        {
            let mut locks = self.deploy_locks.write().await;
            locks.remove(app_name);
        }

        let (name, environment) = requested_deployment_identity(app_name);
        if let Err(e) = self.state_store.delete_app(&name, &environment) {
            tracing::warn!(
                app = app_name,
                "Failed to delete persisted app state: {}",
                e
            );
        }
        let app_root = app_root(&self.runtime.data_dir, app_name);
        if let Err(e) = std::fs::remove_dir_all(&app_root)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                app = app_name,
                path = %app_root.display(),
                "Failed to remove app root: {}",
                e
            );
            return Response::error(format!(
                "Delete partially completed, but failed to remove app files '{}': {}",
                app_root.display(),
                e
            ));
        }

        Response::ok(serde_json::json!({
            "status": "deleted",
            "app": app_name,
            "existed": existed
        }))
    }

    async fn get_status(&self, app_name: &str) -> Response {
        let app = match self.app_manager.get_app(app_name) {
            Some(app) => app,
            None => return Response::error(format!("App not found: {}", app_name)),
        };

        let instances: Vec<InstanceStatus> =
            app.get_instances().iter().map(|i| i.status()).collect();
        let builds = collect_running_build_statuses(&app);

        let status = AppStatus {
            name: app.name(),
            version: app.version(),
            instances,
            builds,
            state: app.state(),
            last_error: app.last_error(),
        };

        Response::ok(status)
    }

    async fn list_apps(&self) -> Response {
        let apps: Vec<serde_json::Value> = self
            .app_manager
            .list_apps()
            .iter()
            .filter_map(|name| {
                self.app_manager.get_app(name).map(|app| {
                    serde_json::json!({
                        "name": app.name(),
                        "version": app.version(),
                        "state": app.state(),
                        "instances": app.get_instances().len()
                    })
                })
            })
            .collect();

        Response::ok(serde_json::json!({ "apps": apps }))
    }

    async fn list_releases(&self, app_name: &str) -> Response {
        let _app = match self.app_manager.get_app(app_name) {
            Some(app) => app,
            None => return Response::error(format!("App not found: {}", app_name)),
        };

        let app_root = self.runtime.data_dir.join("apps").join(app_name);
        let releases_root = app_root.join("releases");
        let current_version = current_release_version(&app_root);

        let mut releases = Vec::new();
        let entries = match std::fs::read_dir(&releases_root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Response::ok(ListReleasesResponse {
                    app: app_name.to_string(),
                    releases,
                });
            }
            Err(error) => {
                return Response::error(format!(
                    "Failed to read releases directory '{}': {}",
                    releases_root.display(),
                    error
                ));
            }
        };

        for entry in entries.flatten() {
            let release_root = entry.path();
            if !release_root.is_dir() {
                continue;
            }

            let Some(version) = entry.file_name().to_str().map(|value| value.to_string()) else {
                continue;
            };

            let manifest_path = release_root.join("app.json");
            let (commit_message, git_dirty) = read_release_manifest_metadata(&manifest_path);
            releases.push(ReleaseInfo {
                current: current_version.as_deref() == Some(version.as_str()),
                deployed_at_unix_secs: directory_modified_unix_secs(&release_root),
                version,
                commit_message,
                git_dirty,
            });
        }

        releases.sort_by(|a, b| {
            b.deployed_at_unix_secs
                .cmp(&a.deployed_at_unix_secs)
                .then_with(|| b.version.cmp(&a.version))
        });

        Response::ok(ListReleasesResponse {
            app: app_name.to_string(),
            releases,
        })
    }

    async fn list_routes(&self) -> Response {
        let route_table = self.routes.read().await;
        let routes: Vec<serde_json::Value> = self
            .app_manager
            .list_apps()
            .iter()
            .map(|app| {
                let patterns = route_table.routes_for_app(app);
                serde_json::json!({ "app": app, "routes": patterns })
            })
            .collect();
        Response::ok(serde_json::json!({ "routes": routes }))
    }

    async fn rollback_app(&self, app_name: &str, version: &str) -> Response {
        let _app = match self.app_manager.get_app(app_name) {
            Some(app) => app,
            None => return Response::error(format!("App not found: {}", app_name)),
        };

        let app_root = self.runtime.data_dir.join("apps").join(app_name);
        let target_path = app_root.join("releases").join(version);

        if !target_path.is_dir() {
            return Response::error(format!(
                "Release '{}' not found for app '{}'",
                version, app_name
            ));
        }

        let routes = {
            let route_table = self.routes.read().await;
            route_table.routes_for_app(app_name)
        };
        if routes.is_empty() {
            return Response::error(format!(
                "Cannot rollback '{}': no routes are configured",
                app_name
            ));
        }

        self.deploy_app(
            app_name,
            version,
            &target_path.to_string_lossy(),
            routes,
            None,
        )
        .await
    }

    async fn update_secrets(
        &self,
        app_name: &str,
        new_secrets: HashMap<String, String>,
    ) -> Response {
        tracing::info!(app = app_name, "Updating secrets");

        if let Err(e) = self.state_store.set_secrets(app_name, &new_secrets) {
            return Response::error(format!("Failed to store secrets: {}", e));
        }

        if let Some(app) = self.app_manager.get_app(app_name) {
            let mut config = app.config.read().clone();
            config.secrets = new_secrets;
            app.update_config(config.clone());
            self.persist_app_state(app_name).await;

            if !app.get_instances().is_empty() {
                let previous_state = app.state();
                app.set_state(AppState::Deploying);
                let rolling_config = RollingUpdateConfig::default();
                let updater =
                    RollingUpdater::new(self.app_manager.spawner().clone(), rolling_config);
                let target =
                    target_new_instances_for_build(config.min_instances, app.get_instances().len());
                match updater.update(&app, config, target).await {
                    Ok(result) if result.success => {
                        app.set_state(AppState::Running);
                        return Response::ok(serde_json::json!({
                            "status": "updated",
                            "app": app_name,
                            "restarted": true
                        }));
                    }
                    Ok(result) => {
                        app.set_state(previous_state);
                        return Response::error(format!(
                            "Rolling restart failed: {:?}",
                            result.error
                        ));
                    }
                    Err(e) => {
                        app.set_state(AppState::Error);
                        return Response::error(format!("Rolling restart failed: {}", e));
                    }
                }
            }
        }

        Response::ok(serde_json::json!({
            "status": "updated",
            "app": app_name,
            "restarted": false
        }))
    }

    pub async fn request_certificate(&self, domain: &str) -> Response {
        let acme_guard = self.acme_client.read().await;
        let acme = match acme_guard.as_ref() {
            Some(acme) => acme,
            None => return Response::error("ACME is disabled".to_string()),
        };

        match acme.request_certificate(domain).await {
            Ok(cert) => Response::ok(serde_json::json!({
                "status": "issued",
                "domain": domain,
                "expires_in_days": cert.days_until_expiry(),
                "cert_path": cert.cert_path.to_string_lossy(),
            })),
            Err(e) => Response::error(format!("Certificate request failed: {}", e)),
        }
    }

    async fn start_on_demand_warm_instance(&self, app: &Arc<App>) -> Result<(), String> {
        let instance = app.allocate_instance();
        let spawner = self.app_manager.spawner();

        match spawner.spawn(app, instance.clone()).await {
            Ok(()) => Ok(()),
            Err(e) => {
                app.remove_instance(&instance.id);
                Err(format!("Warm instance startup failed: {}", e))
            }
        }
    }

    pub(crate) async fn ensure_route_certificate(
        &self,
        app_name: &str,
        domain: &str,
    ) -> Option<CertInfo> {
        if let Some(existing) = self.cert_manager.get_cert_for_host(domain) {
            tracing::debug!(domain = %domain, "Certificate already exists");
            return Some(existing);
        }

        if should_use_self_signed_route_cert(domain) {
            match self.cert_manager.get_or_create_self_signed_cert(domain) {
                Ok(cert) => {
                    tracing::info!(
                        domain = %domain,
                        app = app_name,
                        cert_path = %cert.cert_path.display(),
                        "Generated self-signed certificate for private route domain"
                    );
                    return Some(cert);
                }
                Err(e) => {
                    tracing::warn!(
                        domain = %domain,
                        app = app_name,
                        error = %e,
                        "Failed to generate self-signed certificate for private route domain"
                    );
                    return None;
                }
            }
        }

        let acme_guard = self.acme_client.read().await;
        let acme = acme_guard.as_ref()?;

        tracing::info!(domain = %domain, app = app_name, "Requesting certificate for route");
        match acme.request_certificate(domain).await {
            Ok(cert) => {
                tracing::info!(
                    domain = %domain,
                    expires_in_days = cert.days_until_expiry(),
                    "Certificate issued successfully"
                );
                Some(cert)
            }
            Err(e) => {
                tracing::warn!(
                    domain = %domain,
                    error = %e,
                    "Failed to request certificate (HTTPS may not work for this domain)"
                );
                None
            }
        }
    }
}
