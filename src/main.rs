mod agent;
mod api;
mod apps;
mod auth;
mod backends;
mod config;
mod db;
mod push;
mod scheduler;
mod ws;

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use axum::response::Html;
use axum::routing::get;
use axum::{Json, Router};
use tokio::sync::broadcast;
use tracing::{info, warn};

use crate::agent::{Agent, AgentEvent};
use crate::config::Config;
use crate::db::Db;
use crate::ws::ServerEvent;

/// The shell page is embedded at compile time — no runtime path discovery.
const SHELL_PAGE: &str = include_str!("../static/shell.html");
const APP_ICON: &str = include_str!("../static/icon.svg");
const SERVICE_WORKER: &str = include_str!("../static/sw.js");
const PWA_MANIFEST: &str = r##"{
  "name": "liquid",
  "short_name": "liquid",
  "description": "Your personal software factory",
  "start_url": "/",
  "display": "standalone",
  "background_color": "#101014",
  "theme_color": "#101014",
  "icons": [
    { "src": "/icon.svg", "sizes": "any", "type": "image/svg+xml", "purpose": "any" },
    { "src": "/icon.svg", "sizes": "any", "type": "image/svg+xml", "purpose": "maskable" }
  ]
}"##;

/// Workspace template, embedded so first-run init needs no runtime paths.
const WORKSPACE_TEMPLATE: &[(&str, &str)] = &[
    ("MYSELF.md", include_str!("../default-workspace/MYSELF.md")),
    ("MYHUMAN.md", include_str!("../default-workspace/MYHUMAN.md")),
    ("MEMORY.md", include_str!("../default-workspace/MEMORY.md")),
    (".gitignore", include_str!("../default-workspace/.gitignore")),
    ("PULSE.json", include_str!("../default-workspace/PULSE.json")),
    ("CRONS.json", include_str!("../default-workspace/CRONS.json")),
];

// Phase 0 binds localhost only; your reverse proxy (with SSO) fronts it.
const BIND_ADDR: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

const CLIENT_EVENT_CAPACITY: usize = 1024;

pub type ProxyClient = hyper_util::client::legacy::Client<
    hyper_util::client::legacy::connect::HttpConnector,
    axum::body::Body,
>;

#[derive(Clone)]
pub struct AppState {
    pub db: Db,
    pub agent: Agent,
    pub client_events: broadcast::Sender<ServerEvent>,
    pub workspace_dir: PathBuf,
    pub apps_cache: Arc<Mutex<Vec<apps::AppManifest>>>,
    pub backends: Arc<backends::BackendManager>,
    pub http_client: ProxyClient,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "liquid=info".into()),
        )
        .init();

    let config = Config::from_env()?;
    init_workspace(&config.workspace_dir)?;

    let db = Db::open(&config.data_dir.join("liquid.db"))?;
    let agent = Agent::start(&config);
    let (client_events, _) = broadcast::channel(CLIENT_EVENT_CAPACITY);

    let backends = backends::BackendManager::new(config.workspace_dir.clone(), config.port);
    let http_client: ProxyClient = hyper_util::client::legacy::Client::builder(
        hyper_util::rt::TokioExecutor::new(),
    )
    .build_http();

    let initial_apps = apps::scan_apps(&config.workspace_dir);
    sync_backends(&backends, &initial_apps);
    let state = AppState {
        db,
        agent,
        client_events,
        workspace_dir: config.workspace_dir.clone(),
        apps_cache: Arc::new(Mutex::new(initial_apps)),
        backends,
        http_client,
    };

    tokio::spawn(record_agent_events(state.clone()));
    scheduler::start(state.clone());

    let protected = Router::new()
        .route("/api/conversations", get(api::list_conversations))
        .route(
            "/api/conversations/{id}",
            axum::routing::delete(api::delete_conversation),
        )
        .route(
            "/api/conversations/{id}/messages",
            get(api::list_messages),
        )
        .route("/api/apps", get(apps::list_apps))
        .route("/api/apps/{app}/log", get(apps::app_log))
        .route(
            "/api/kv/{app}/{key}",
            get(apps::kv_get)
                .put(apps::kv_put)
                .delete(apps::kv_delete),
        )
        .route("/api/kv/{app}", get(apps::kv_list))
        .route("/api/shell", get(apps::shell_get).put(apps::shell_put))
        .route("/api/push/key", get(push::public_key))
        .route("/api/push/subscribe", axum::routing::post(push::subscribe))
        .route(
            "/api/push/unsubscribe",
            axum::routing::post(push::unsubscribe),
        )
        .route("/api/push/test", axum::routing::post(push::test))
        .route("/ws", get(ws::ws_handler))
        .route_layer(axum::middleware::from_fn_with_state(
            state.clone(),
            api::require_auth,
        ));

    let app = Router::new()
        .route("/", get(|| async { Html(SHELL_PAGE) }))
        .route(
            "/manifest.webmanifest",
            get(|| async {
                (
                    [(axum::http::header::CONTENT_TYPE, "application/manifest+json")],
                    PWA_MANIFEST,
                )
            }),
        )
        .route(
            "/icon.svg",
            get(|| async {
                ([(axum::http::header::CONTENT_TYPE, "image/svg+xml")], APP_ICON)
            }),
        )
        .route(
            "/sw.js",
            get(|| async {
                (
                    [
                        (axum::http::header::CONTENT_TYPE, "text/javascript"),
                        (axum::http::header::CACHE_CONTROL, "no-cache"),
                    ],
                    SERVICE_WORKER,
                )
            }),
        )
        .route(
            "/api/health",
            get(|| async { Json(serde_json::json!({ "status": "ok" })) }),
        )
        .route("/api/auth/status", get(api::auth_status))
        .route("/api/auth/setup", axum::routing::post(api::auth_setup))
        .route("/api/auth/login", axum::routing::post(api::auth_login))
        // App static files are public: iframes and their relative asset
        // fetches can't attach auth headers. Data stays behind /api auth.
        // Backend proxy first — the static "api" segment outranks the
        // wildcard, so /app/x/api/* reaches the app's backend.
        .route(
            "/app/{app}/api",
            axum::routing::any(backends::proxy_api_root),
        )
        .route(
            "/app/{app}/api/",
            axum::routing::any(backends::proxy_api_root),
        )
        .route(
            "/app/{app}/api/{*path}",
            axum::routing::any(backends::proxy_api),
        )
        .route("/app/{app}/", get(apps::serve_app_index))
        .route("/app/{app}/{*path}", get(apps::serve_app_file))
        .merge(protected)
        .with_state(state);

    let addr = SocketAddr::new(BIND_ADDR, config.port);
    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    info!("listening on http://{addr}");
    println!("__READY__");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")
}

/// Consumes the agent event stream: persists finished assistant turns and
/// session ids, and fans events out to connected clients. Runs for the
/// lifetime of the process so history is recorded even with no client
/// connected (e.g. a query finishing after the phone locked).
async fn record_agent_events(state: AppState) {
    let mut events = state.agent.subscribe();
    let mut buffers: HashMap<i64, String> = HashMap::new();

    loop {
        let event = match events.recv().await {
            Ok(event) => event,
            Err(broadcast::error::RecvError::Lagged(missed)) => {
                warn!("recorder lagged, {missed} agent events lost");
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => return,
        };

        let conversation_id = match conversation_id_of(&event) {
            Some(id) => id,
            None => continue,
        };

        let server_event = match event {
            AgentEvent::Token { text, .. } => {
                buffers.entry(conversation_id).or_default().push_str(&text);
                ServerEvent::Token {
                    conversation_id,
                    text,
                }
            }
            AgentEvent::Tool { name, status, .. } => ServerEvent::Tool {
                conversation_id,
                name,
                status,
            },
            AgentEvent::Session { session_id, .. } => {
                if let Err(err) = state
                    .db
                    .set_conversation_session(conversation_id, &session_id)
                {
                    warn!("failed to persist session id: {err:#}");
                }
                continue;
            }
            AgentEvent::Shell { action, app, .. } => ServerEvent::ShellCommand { action, app },
            AgentEvent::Notify { title, body, .. } => {
                push::notify_all(&state, &title, &body).await;
                ServerEvent::Notify { title, body }
            }
            AgentEvent::Error { message, .. } => {
                if let Err(err) = state.db.append_message(conversation_id, "error", &message) {
                    warn!("failed to persist error message: {err:#}");
                }
                ServerEvent::Error {
                    conversation_id,
                    message,
                }
            }
            AgentEvent::Done {
                used_file_tools, ..
            } => {
                let content = buffers.remove(&conversation_id).unwrap_or_default();
                if !content.is_empty() {
                    if let Err(err) = state.db.append_message(conversation_id, "assistant", &content) {
                        warn!("failed to persist assistant message: {err:#}");
                    }
                    // Scheduled runs finish while nobody's watching — push them.
                    let scheduler_conversation = state
                        .db
                        .get_setting("scheduler_conversation_id")
                        .ok()
                        .flatten()
                        .and_then(|v| v.parse::<i64>().ok());
                    if scheduler_conversation == Some(conversation_id) {
                        let snippet: String = content.chars().take(140).collect();
                        push::notify_all(&state, "liquid ⏰", &snippet).await;
                    }
                }
                // The agent touched files — apps and backends may have changed.
                if used_file_tools {
                    let fresh = apps::scan_apps(&state.workspace_dir);
                    // Always reconcile backends: a file edit inside an
                    // existing backend changes no manifest but needs a restart.
                    sync_backends(&state.backends, &fresh);
                    let changed = {
                        let mut cache = state.apps_cache.lock().expect("apps cache poisoned");
                        if *cache == fresh {
                            false
                        } else {
                            *cache = fresh;
                            true
                        }
                    };
                    if changed {
                        let _ = state.client_events.send(ServerEvent::AppsChanged {
                            apps: apps::enriched_apps(&state),
                        });
                    }
                }
                ServerEvent::Done { conversation_id }
            }
        };
        // No connected clients is fine; the recorder already persisted.
        let _ = state.client_events.send(server_event);
    }
}

fn sync_backends(manager: &Arc<backends::BackendManager>, apps: &[apps::AppManifest]) {
    let with_backend: Vec<String> = apps
        .iter()
        .filter(|app| app.has_backend)
        .map(|app| app.id.clone())
        .collect();
    manager.sync(&with_backend);
}

fn conversation_id_of(event: &AgentEvent) -> Option<i64> {
    let id = match event {
        AgentEvent::Token { id, .. }
        | AgentEvent::Tool { id, .. }
        | AgentEvent::Done { id, .. }
        | AgentEvent::Error { id, .. }
        | AgentEvent::Session { id, .. }
        | AgentEvent::Shell { id, .. }
        | AgentEvent::Notify { id, .. } => id,
    };
    match id.parse() {
        Ok(id) => Some(id),
        Err(_) => {
            warn!("agent event with non-numeric id: {id}");
            None
        }
    }
}

/// Resolves on SIGINT or SIGTERM. The agent child has kill_on_drop, so
/// letting main return tears the whole tree down.
async fn shutdown_signal() {
    let ctrl_c = tokio::signal::ctrl_c();
    let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .expect("installing SIGTERM handler");
    tokio::select! {
        _ = ctrl_c => info!("received SIGINT, shutting down"),
        _ = sigterm.recv() => info!("received SIGTERM, shutting down"),
    }
}

/// First-run workspace init: create the directory, seed the memory-file
/// template, `git init`, and make the initial commit — versioned from the
/// very first byte. Existing workspaces are left untouched.
fn init_workspace(dir: &Path) -> anyhow::Result<()> {
    if !dir.exists() {
        std::fs::create_dir_all(dir.join("memory"))
            .with_context(|| format!("creating {}", dir.display()))?;
        for (name, contents) in WORKSPACE_TEMPLATE {
            std::fs::write(dir.join(name), contents)
                .with_context(|| format!("seeding {name}"))?;
        }
        info!("created workspace at {} from template", dir.display());
    }
    if !dir.join(".git").exists() {
        match git(dir, &["init", "--initial-branch=main"]) {
            Ok(()) => {
                info!("initialized git repo in workspace");
                // Repo-local identity so commits work even for a service user
                // with no global git config. The agent commits as itself.
                let _ = git(dir, &["config", "user.name", "liquid"]);
                let _ = git(dir, &["config", "user.email", "liquid@localhost"]);
                if git(dir, &["add", "-A"]).is_ok() {
                    match git(dir, &["commit", "-m", "Initialize liquid workspace"]) {
                        Ok(()) => info!("workspace initial commit created"),
                        Err(err) => warn!("initial commit failed: {err}"),
                    }
                }
            }
            Err(err) => warn!("git init failed ({err}); workspace is not versioned"),
        }
    }
    Ok(())
}

fn git(dir: &Path, args: &[&str]) -> anyhow::Result<()> {
    let status = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .context("running git")?;
    anyhow::ensure!(status.success(), "git {args:?} exited with {status}");
    Ok(())
}
