//! Per-app backend processes (ADR 0002): each app declares how its server
//! runs (`backend.run` in app.json, an argv vector — `["mix", "phx.server"]`,
//! `["bun", "run", "backend/index.ts"]`, …); `backend/index.ts` with no
//! declaration keeps the zero-config Bun default. The supervisor allocates a
//! port, injects it (plus `LIQUID_APP_ID` / `LIQUID_APP_DATA_DIR`) via env,
//! polls readiness (TCP connect, or HTTP GET on a declared `backend.health`
//! path), restarts on file change, and applies a 3-strike crash policy. The
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
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::watch;
use tracing::{error, info, warn};

use crate::apps::BackendSpec;
use crate::AppState;

/// Backend ports are allocated upward from supervisor port + this offset.
const PORT_OFFSET: u16 = 10;
const MAX_FAST_RESTARTS: u32 = 3;
const STABLE_UPTIME: Duration = Duration::from_secs(30);
const RESTART_DELAY: Duration = Duration::from_secs(1);
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(300);
/// Fast-poll window: ~15s of 300ms probes after spawn.
const HEALTH_FAST_ATTEMPTS: u32 = 50;
/// After the fast window, probe every Nth tick (~3s) — a compiling backend
/// (mix, go build) can take minutes to listen and must still flip to Running.
const HEALTH_SLOW_EVERY: u32 = 10;
const HEALTH_HTTP_TIMEOUT: Duration = Duration::from_secs(2);
/// Directories whose contents (and mtimes) never count as code changes:
/// runtime state and dependency/build output must not restart the backend.
const WATCH_IGNORE: &[&str] = &["node_modules", "data", "_build", "deps", ".git"];

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

/// One app that should have a running backend: its id and how to run it.
#[derive(Clone, Debug, PartialEq)]
pub struct BackendTarget {
    pub id: String,
    pub spec: BackendSpec,
}

struct BackendHandle {
    port: u16,
    /// Bumped to force a respawn (files or the declared spec changed); carries
    /// the spec so the runner task always spawns the current declaration.
    restart_tx: watch::Sender<(u64, BackendSpec)>,
    shutdown_tx: watch::Sender<bool>,
    dir_mtime: SystemTime,
    spec: BackendSpec,
}

pub struct BackendManager {
    served_dir: PathBuf,
    base_port: u16,
    backends: Mutex<HashMap<String, BackendHandle>>,
    next_offset: Mutex<u16>,
    /// Ports freed when apps were removed, reused before allocating new ones.
    freed_ports: Mutex<Vec<u16>>,
    statuses: Arc<Mutex<HashMap<String, BackendStatus>>>,
}

impl BackendManager {
    pub fn new(served_dir: PathBuf, supervisor_port: u16) -> Arc<Self> {
        Arc::new(Self {
            served_dir,
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
    /// one: spawn new, stop removed, restart changed (files OR declared spec).
    /// Called at boot and after every agent query that touched files.
    pub fn sync(self: &Arc<Self>, targets: &[BackendTarget]) {
        let mut backends = self.backends.lock().expect("backends poisoned");

        // Stop backends whose app (or backend declaration) is gone.
        let removed: Vec<String> = backends
            .keys()
            .filter(|id| !targets.iter().any(|t| &t.id == *id))
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

        for target in targets {
            let app_dir = self.served_dir.join("apps").join(&target.id);
            let mtime = dir_mtime_max(&app_dir);
            match backends.get_mut(&target.id) {
                Some(handle) => {
                    let spec_changed = handle.spec != target.spec;
                    if mtime > handle.dir_mtime || spec_changed {
                        info!(
                            "backend {} changed ({}); restarting",
                            target.id,
                            if spec_changed { "declaration" } else { "files" }
                        );
                        handle.dir_mtime = mtime;
                        handle.spec = target.spec.clone();
                        handle
                            .restart_tx
                            .send_modify(|(n, spec)| { *n += 1; *spec = target.spec.clone(); });
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
                    let (restart_tx, restart_rx) = watch::channel((0u64, target.spec.clone()));
                    let (shutdown_tx, shutdown_rx) = watch::channel(false);
                    backends.insert(
                        target.id.clone(),
                        BackendHandle {
                            port,
                            restart_tx,
                            shutdown_tx,
                            dir_mtime: mtime,
                            spec: target.spec.clone(),
                        },
                    );
                    info!("starting backend for {} on port {port}: {:?}", target.id, target.spec.run);
                    tokio::spawn(run_backend(
                        target.id.clone(),
                        app_dir,
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
    UrlPath((app, path)): UrlPath<(String, String)>,
    request: axum::extract::Request,
) -> Response {
    // Backends share their app's visibility rule (private = owner session cookie
    // or the screenshot capability; never blanket loopback trust).
    let query = request.uri().query().map(str::to_string);
    if let Some(denied) = crate::apps::check_app_access(&state, &app, request.headers(), query.as_deref()) {
        return denied;
    }
    proxy(state, app, path, request).await
}

pub async fn proxy_api_root(
    State(state): State<AppState>,
    UrlPath(app): UrlPath<String>,
    request: axum::extract::Request,
) -> Response {
    let query = request.uri().query().map(str::to_string);
    if let Some(denied) = crate::apps::check_app_access(&state, &app, request.headers(), query.as_deref()) {
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
/// `WATCH_IGNORE` dirs are skipped entirely (metadata included): runtime
/// writes (data/) and dependency/build output (node_modules/, _build/, deps/)
/// must never look like code changes.
fn dir_mtime_max(dir: &Path) -> SystemTime {
    fn walk(dir: &Path, newest: &mut SystemTime) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for entry in entries.flatten() {
            let name = entry.file_name();
            if WATCH_IGNORE.iter().any(|skip| name == *skip) {
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

/// One readiness probe. Without a declared health path, "something accepts on
/// the port" is ready. With one, it must answer an HTTP GET with a 2xx —
/// hand-rolled HTTP/1.1 over the socket we already have (a full client is
/// overkill for reading one status line).
async fn probe_ready(port: u16, health_path: Option<&str>) -> bool {
    let Ok(mut stream) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await else {
        return false;
    };
    let Some(path) = health_path else { return true };
    let check = async {
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.ok()?;
        // "HTTP/1.1 XXX" — the status code is bytes 9..12.
        let mut head = [0u8; 12];
        stream.read_exact(&mut head).await.ok()?;
        std::str::from_utf8(&head[9..12]).ok()?.parse::<u16>().ok()
    };
    match tokio::time::timeout(HEALTH_HTTP_TIMEOUT, check).await {
        Ok(Some(code)) => (200..300).contains(&code),
        _ => false,
    }
}

/// Owns one app's backend child process for the app's lifetime.
async fn run_backend(
    app_id: String,
    app_dir: PathBuf,
    port: u16,
    mut restart_rx: watch::Receiver<(u64, BackendSpec)>,
    mut shutdown_rx: watch::Receiver<bool>,
    statuses: Arc<Mutex<HashMap<String, BackendStatus>>>,
) {
    let mut fast_restarts: u32 = 0;
    // The documented convention (prompt.ts) is that a backend stores state in
    // data/ (e.g. bun:sqlite at data/app.db). data/ is gitignored, so it isn't
    // in the deployed worktree checkout — ensure it exists before the backend
    // opens a database there, or bun:sqlite fails with SQLITE_CANTOPEN.
    let data_dir = app_dir.join("data");
    if let Err(err) = std::fs::create_dir_all(&data_dir) {
        warn!("backend {app_id}: could not create data/ dir: {err}");
    }
    loop {
        // Always spawn the CURRENT declaration — a restart triggered by a spec
        // change must not relaunch the old command.
        let spec = restart_rx.borrow_and_update().1.clone();
        let (program, args) = match spec.run.split_first() {
            Some(split) => split,
            None => {
                // Unreachable: apps.rs validates run is non-empty. Fail safe.
                error!("backend {app_id}: empty run command");
                set_state(&statuses, &app_id, port, BackendState::Failed);
                return;
            }
        };
        set_state(&statuses, &app_id, port, BackendState::Starting);
        let mut child = match Command::new(program)
            .args(args)
            .current_dir(&app_dir)
            .envs(&spec.env)
            // The platform contract wins over any declared env.
            .env("PORT", port.to_string())
            .env("LIQUID_APP_ID", &app_id)
            .env("LIQUID_APP_DATA_DIR", &data_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                error!("backend {app_id}: failed to spawn {program}: {err}");
                set_state(&statuses, &app_id, port, BackendState::Failed);
                tokio::select! {
                    _ = shutdown_rx.changed() => return,
                    _ = restart_rx.changed() => { fast_restarts = 0; continue; }
                }
            }
        };

        let spawned_at = Instant::now();
        let mut health_ticks = 0u32;
        let mut ready = false;
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
                _ = health_tick.tick(), if !ready => {
                    health_ticks += 1;
                    // Fast-poll at first, then keep probing slowly forever: a
                    // backend that compiles on boot (mix, go) becomes Running
                    // whenever it finally listens.
                    let probe_now = health_ticks <= HEALTH_FAST_ATTEMPTS
                        || health_ticks % HEALTH_SLOW_EVERY == 0;
                    if probe_now && probe_ready(port, spec.health.as_deref()).await {
                        set_state(&statuses, &app_id, port, BackendState::Running);
                        ready = true;
                    } else if health_ticks == HEALTH_FAST_ATTEMPTS {
                        warn!("backend {app_id}: not ready on {port} after startup window; still checking");
                    }
                }
            }
        };

        match exit {
            None => {
                // Deliberate restart (file/spec change) — clean slate.
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
    fn dir_mtime_sees_code_edits_but_ignores_data_and_build_writes() {
        let root = std::env::temp_dir().join(format!("liquid-mtime-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join("data")).unwrap();
        std::fs::create_dir_all(root.join("_build")).unwrap();
        std::fs::write(root.join("index.ts"), "x").unwrap();
        std::fs::write(root.join("sub/util.ts"), "y").unwrap();
        let base = dir_mtime_max(&root);
        assert!(base > SystemTime::UNIX_EPOCH);

        // Writes inside data/ (runtime state) or _build/ (compile output) must
        // NOT look like code changes — they'd restart the backend on every
        // database write / on its own compilation.
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(root.join("data/app.db"), "z").unwrap();
        std::fs::write(root.join("_build/server.beam"), "b").unwrap();
        assert_eq!(dir_mtime_max(&root), base);

        // A code edit MUST bump it.
        std::thread::sleep(Duration::from_millis(1100));
        std::fs::write(root.join("sub/util.ts"), "y2").unwrap();
        assert!(dir_mtime_max(&root) > base);

        std::fs::remove_dir_all(&root).unwrap();
    }
}
