//! Per-app backend processes: `apps/<id>/backend/index.ts` run with Bun on an
//! allocated port, restarted on file change, 3-strike crash policy. The
//! supervisor proxies `/app/<id>/api/*` to the app's port with the prefix
//! stripped (a backend sees `/health`, not `/app/x/api/health`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Serialize;
use serde_json::json;
use tokio::process::Command;
use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::AppState;

pub const BACKEND_ENTRY: &str = "backend/index.ts";
/// Backend ports are allocated upward from supervisor port + this offset.
const PORT_OFFSET: u16 = 10;
const MAX_FAST_RESTARTS: u32 = 3;
const STABLE_UPTIME: Duration = Duration::from_secs(30);
const RESTART_DELAY: Duration = Duration::from_secs(1);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(300);
const HEALTH_POLL_ATTEMPTS: u32 = 50; // ~15s to come up before we stop checking

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendState {
    Starting,
    Running,
    Failed,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct BackendStatus {
    pub port: u16,
    pub state: BackendState,
}

struct BackendHandle {
    port: u16,
    /// Bumped to force a restart (backend files changed).
    restart_tx: watch::Sender<u64>,
    shutdown_tx: watch::Sender<bool>,
    dir_mtime: SystemTime,
}

pub struct BackendManager {
    workspace_dir: PathBuf,
    base_port: u16,
    backends: Mutex<HashMap<String, BackendHandle>>,
    next_offset: Mutex<u16>,
    /// Ports freed when apps were removed, reused before allocating new ones.
    freed_ports: Mutex<Vec<u16>>,
    statuses: Arc<Mutex<HashMap<String, BackendStatus>>>,
}

impl BackendManager {
    pub fn new(workspace_dir: PathBuf, supervisor_port: u16) -> Arc<Self> {
        Arc::new(Self {
            workspace_dir,
            base_port: supervisor_port.saturating_add(PORT_OFFSET),
            backends: Mutex::new(HashMap::new()),
            next_offset: Mutex::new(0),
            freed_ports: Mutex::new(Vec::new()),
            statuses: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    pub fn status(&self, app_id: &str) -> Option<BackendStatus> {
        self.statuses.lock().expect("statuses poisoned").get(app_id).copied()
    }

    /// Reconcile running backends against the current set of apps that have
    /// one: spawn new, stop removed, restart changed. Called at boot and
    /// after every agent query that touched files.
    pub fn sync(self: &Arc<Self>, app_ids_with_backend: &[String]) {
        let mut backends = self.backends.lock().expect("backends poisoned");

        // Stop backends whose app (or backend dir) is gone.
        let removed: Vec<String> = backends
            .keys()
            .filter(|id| !app_ids_with_backend.contains(id))
            .cloned()
            .collect();
        for id in removed {
            info!("stopping backend for removed app {id}");
            if let Some(handle) = backends.remove(&id) {
                let _ = handle.shutdown_tx.send(true);
                // Reclaim the port so a churn of apps doesn't leak upward.
                self.freed_ports.lock().expect("freed_ports poisoned").push(handle.port);
            }
            self.statuses.lock().expect("statuses poisoned").remove(&id);
        }

        for id in app_ids_with_backend {
            let backend_dir = self.workspace_dir.join("apps").join(id).join("backend");
            let mtime = dir_mtime_max(&backend_dir);
            match backends.get_mut(id) {
                Some(handle) => {
                    if mtime > handle.dir_mtime {
                        info!("backend files changed for {id}; restarting");
                        handle.dir_mtime = mtime;
                        handle.restart_tx.send_modify(|n| *n += 1);
                    }
                }
                None => {
                    let port = self
                        .freed_ports
                        .lock()
                        .expect("freed_ports poisoned")
                        .pop()
                        .unwrap_or_else(|| {
                            let mut offset = self.next_offset.lock().expect("offset poisoned");
                            let port = self.base_port.saturating_add(*offset);
                            *offset += 1;
                            port
                        });
                    let (restart_tx, restart_rx) = watch::channel(0u64);
                    let (shutdown_tx, shutdown_rx) = watch::channel(false);
                    backends.insert(
                        id.clone(),
                        BackendHandle { port, restart_tx, shutdown_tx, dir_mtime: mtime },
                    );
                    info!("starting backend for {id} on port {port}");
                    tokio::spawn(run_backend(
                        id.clone(),
                        self.workspace_dir.join("apps").join(id),
                        port,
                        restart_rx,
                        shutdown_rx,
                        Arc::clone(&self.statuses),
                    ));
                }
            }
        }
    }
}

// --- reverse proxy: /app/{app}/api/* → the app's backend port ---------------------

pub async fn proxy_api(
    State(state): State<AppState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    UrlPath((app, path)): UrlPath<(String, String)>,
    request: axum::extract::Request,
) -> Response {
    // Backends share their app's visibility rule (private = owner cookie or loopback).
    if let Some(denied) = crate::apps::check_app_access(&state, &app, peer, request.headers()) {
        return denied;
    }
    proxy(state, app, path, request).await
}

pub async fn proxy_api_root(
    State(state): State<AppState>,
    axum::extract::ConnectInfo(peer): axum::extract::ConnectInfo<std::net::SocketAddr>,
    UrlPath(app): UrlPath<String>,
    request: axum::extract::Request,
) -> Response {
    if let Some(denied) = crate::apps::check_app_access(&state, &app, peer, request.headers()) {
        return denied;
    }
    proxy(state, app, String::new(), request).await
}

async fn proxy(state: AppState, app: String, path: String, mut request: axum::extract::Request) -> Response {
    let Some(status) = state.backends.status(&app) else {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({ "error": "this app has no backend" })),
        )
            .into_response();
    };
    let query = request
        .uri()
        .query()
        .map(|q| format!("?{q}"))
        .unwrap_or_default();
    let target = format!("http://127.0.0.1:{}/{path}{query}", status.port);
    let Ok(uri) = target.parse() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    *request.uri_mut() = uri;
    // The backend should see itself as the host, not the shell's domain.
    request.headers_mut().remove(axum::http::header::HOST);

    match state.http_client.request(request).await {
        Ok(response) => response.map(axum::body::Body::new).into_response(),
        Err(err) => {
            warn!("proxy to backend {app} failed: {err}");
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({
                    "error": "backend unreachable",
                    "state": status.state,
                })),
            )
                .into_response()
        }
    }
}

/// Newest mtime anywhere under `dir` — cheap change detection for restart.
/// `data/` and `node_modules/` are ignored entirely (metadata included):
/// runtime writes there must never look like code changes.
fn dir_mtime_max(dir: &Path) -> SystemTime {
    fn walk(dir: &Path, newest: &mut SystemTime) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if name == "node_modules" || name == "data" {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if let Ok(mtime) = meta.modified() {
                if mtime > *newest {
                    *newest = mtime;
                }
            }
            if meta.is_dir() {
                walk(&entry.path(), newest);
            }
        }
    }
    let mut newest = SystemTime::UNIX_EPOCH;
    walk(dir, &mut newest);
    newest
}

fn set_state(
    statuses: &Mutex<HashMap<String, BackendStatus>>,
    app_id: &str,
    port: u16,
    state: BackendState,
) {
    statuses
        .lock()
        .expect("statuses poisoned")
        .insert(app_id.to_string(), BackendStatus { port, state });
}

/// Owns one app's backend child process for the app's lifetime.
async fn run_backend(
    app_id: String,
    app_dir: PathBuf,
    port: u16,
    mut restart_rx: watch::Receiver<u64>,
    mut shutdown_rx: watch::Receiver<bool>,
    statuses: Arc<Mutex<HashMap<String, BackendStatus>>>,
) {
    let mut fast_restarts: u32 = 0;
    // The documented convention (prompt.ts) is that a backend stores state in
    // data/ (e.g. bun:sqlite at data/app.db). data/ is gitignored, so it isn't
    // in the deployed worktree checkout — ensure it exists before the backend
    // opens a database there, or bun:sqlite fails with SQLITE_CANTOPEN.
    if let Err(err) = std::fs::create_dir_all(app_dir.join("data")) {
        warn!("backend {app_id}: could not create data/ dir: {err}");
    }
    loop {
        set_state(&statuses, &app_id, port, BackendState::Starting);
        let mut child = match Command::new("bun")
            .arg("run")
            .arg(BACKEND_ENTRY)
            .current_dir(&app_dir)
            .env("PORT", port.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                error!("backend {app_id}: failed to spawn bun: {err}");
                set_state(&statuses, &app_id, port, BackendState::Failed);
                tokio::select! {
                    _ = shutdown_rx.changed() => return,
                    _ = restart_rx.changed() => { fast_restarts = 0; continue; }
                }
            }
        };

        let spawned_at = Instant::now();
        let mut health_attempts = 0u32;
        let mut health_tick = tokio::time::interval(HEALTH_POLL_INTERVAL);

        // Some(status) = child exited on its own; None = we killed it on purpose.
        let exit = loop {
            tokio::select! {
                status = child.wait() => break Some(status),
                _ = shutdown_rx.changed() => {
                    let _ = child.kill().await;
                    return;
                }
                _ = restart_rx.changed() => {
                    let _ = child.kill().await;
                    break None;
                }
                _ = health_tick.tick(), if health_attempts < HEALTH_POLL_ATTEMPTS => {
                    health_attempts += 1;
                    if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
                        set_state(&statuses, &app_id, port, BackendState::Running);
                        health_attempts = HEALTH_POLL_ATTEMPTS; // stop polling
                    } else if health_attempts == HEALTH_POLL_ATTEMPTS {
                        warn!("backend {app_id}: nothing listening on {port} after startup window");
                    }
                }
            }
        };

        match exit {
            None => {
                // Deliberate restart (file change) — clean slate.
                fast_restarts = 0;
                continue;
            }
            Some(status) => {
                if spawned_at.elapsed() >= STABLE_UPTIME {
                    fast_restarts = 0;
                } else {
                    fast_restarts += 1;
                }
                warn!("backend {app_id} exited ({status:?}); fast restarts: {fast_restarts}");
                if fast_restarts > MAX_FAST_RESTARTS {
                    error!(
                        "backend {app_id} is crash-looping; marked failed until its files change"
                    );
                    set_state(&statuses, &app_id, port, BackendState::Failed);
                    tokio::select! {
                        _ = shutdown_rx.changed() => return,
                        _ = restart_rx.changed() => { fast_restarts = 0; continue; }
                    }
                }
                tokio::time::sleep(RESTART_DELAY).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_mtime_of_missing_dir_is_epoch() {
        assert_eq!(dir_mtime_max(Path::new("/nonexistent/liquid")), SystemTime::UNIX_EPOCH);
    }

    #[test]
    fn dir_mtime_sees_code_edits_but_ignores_data_writes() {
        let root = std::env::temp_dir().join(format!("liquid-mtime-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::write(root.join("index.ts"), "x").unwrap();
        std::fs::write(root.join("sub/util.ts"), "y").unwrap();
        let base = dir_mtime_max(&root);
        assert!(base > SystemTime::UNIX_EPOCH);

        // A write inside data/ must NOT look like a code change (it would
        // restart the backend on every database write).
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(root.join("data/app.db"), "z").unwrap();
        assert_eq!(dir_mtime_max(&root), base);

        // A code edit MUST bump it.
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(root.join("sub/util.ts"), "y2").unwrap();
        assert!(dir_mtime_max(&root) > base);

        std::fs::remove_dir_all(&root).unwrap();
    }
}
