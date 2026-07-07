mod agent;
mod api;
mod apps;
mod auth;
mod backends;
mod config;
mod db;
mod deploy;
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
    /// Where the agent works and commits. Memory files, SHELL.json, CRONS are
    /// read live from here (never gated by the pipeline).
    pub workspace_dir: PathBuf,
    /// Where apps are served from and backends run: the deployed worktree.
    pub served_dir: PathBuf,
    pub apps_cache: Arc<Mutex<Vec<apps::AppManifest>>>,
    pub backends: Arc<backends::BackendManager>,
    pub deploy: Arc<deploy::DeployManager>,
    pub review_command: Vec<String>,
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

    let saved_mode = db.get_setting("pipeline_mode").ok().flatten();
    let deploy = Arc::new(
        deploy::DeployManager::init(
            &config.workspace_dir,
            &config.data_dir,
            config.pipeline_mode,
            saved_mode,
        )
        .context("initializing deploy pipeline")?,
    );
    deploy.persist_mode(&db);
    let served_dir = deploy.served_dir().to_path_buf();

    let backends = backends::BackendManager::new(served_dir.clone(), config.port);
    let http_client: ProxyClient = hyper_util::client::legacy::Client::builder(
        hyper_util::rt::TokioExecutor::new(),
    )
    .build_http();

    // Apps are served from the deployed worktree, not the live workspace.
    let initial_apps = apps::scan_apps(&served_dir);
    sync_backends(&backends, &initial_apps);
    let state = AppState {
        db,
        agent,
        client_events,
        workspace_dir: config.workspace_dir.clone(),
        served_dir,
        apps_cache: Arc::new(Mutex::new(initial_apps)),
        backends,
        deploy,
        review_command: config.review_command.clone(),
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
        .route(
            "/api/auth/change_password",
            axum::routing::post(api::auth_change_password),
        )
        .route("/api/settings", get(api::get_settings).put(api::put_settings))
        .route("/api/apps", get(apps::list_apps))
        .route("/api/apps/{app}/log", get(apps::app_log))
        .route(
            "/api/apps/{app}/graduate",
            axum::routing::post(apps::graduate),
        )
        .route(
            "/api/kv/{app}/{key}",
            get(apps::kv_get)
                .put(apps::kv_put)
                .delete(apps::kv_delete),
        )
        .route("/api/kv/{app}", get(apps::kv_list))
        .route("/api/shell", get(apps::shell_get).put(apps::shell_put))
        .route(
            "/api/pipeline",
            get(api::pipeline_status).put(api::set_pipeline_mode),
        )
        .route(
            "/api/pipeline/approve",
            axum::routing::post(api::pipeline_approve),
        )
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
        .route("/api/health", get(health))
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
    // Which conversation the single agent worker is currently processing.
    let mut busy_conversation: Option<i64> = None;

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

        // Any activity on a new conversation means the worker started it.
        if busy_conversation != Some(conversation_id) {
            busy_conversation = Some(conversation_id);
            broadcast_busy(&state, conversation_id, true);
        }

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
                // The agent touched files — run the deploy pipeline, which
                // decides whether the change goes live now (vibe / non-app) or
                // needs review (reviewed mode + apps/ changed).
                if used_file_tools {
                    reconcile_deploy(&state, conversation_id);
                }
                busy_conversation = None;
                broadcast_busy(&state, conversation_id, false);
                ServerEvent::Done { conversation_id }
            }
        };
        // No connected clients is fine; the recorder already persisted.
        let _ = state.client_events.send(server_event);
    }
}

/// Health + a snapshot of platform state, handy for debugging a deploy.
async fn health(axum::extract::State(state): axum::extract::State<AppState>) -> Json<serde_json::Value> {
    let deployed = state.deploy.deployed_commit().unwrap_or_default();
    let app_count = state.apps_cache.lock().expect("apps cache poisoned").len();
    Json(serde_json::json!({
        "status": "ok",
        "pipeline_mode": state.deploy.mode(),
        "pipeline_status": state.deploy.status(),
        "deployed_commit": deployed,
        "app_count": app_count,
    }))
}

fn broadcast_busy(state: &AppState, conversation_id: i64, busy: bool) {
    let title = state
        .db
        .conversation_title(conversation_id)
        .ok()
        .flatten()
        .unwrap_or_default();
    let _ = state.client_events.send(ServerEvent::AgentBusy {
        conversation_id,
        title,
        busy,
    });
}

/// Run the pipeline after the agent commits, then refresh served apps.
/// `origin_conversation` is where a rejection notice is posted.
fn reconcile_deploy(state: &AppState, origin_conversation: i64) {
    match state.deploy.reconcile() {
        Ok(None) => refresh_served_apps(state), // deployed (or nothing to do)
        Ok(Some(candidate)) => {
            // reviewed mode: an app change is gated. Run the reviewer in its
            // own task — a review is a subprocess + model call and must not
            // block the recorder loop (which streams every conversation).
            let state = state.clone();
            tokio::spawn(async move {
                review_candidate(&state, &candidate, origin_conversation).await;
            });
        }
        Err(err) => warn!("deploy reconcile failed: {err:#}"),
    }
}

/// Run the reviewer subagent on a gated candidate commit and act on its
/// verdict. The diff is computed by the supervisor (never supplied by the
/// agent); the review record lands in supervisor-owned pipeline storage.
async fn review_candidate(state: &AppState, candidate: &str, origin_conversation: i64) {
    let _ = state.client_events.send(ServerEvent::Pipeline {
        status: state.deploy.status(),
    });

    let diff = match state.deploy.review_diff() {
        Ok(diff) => diff,
        Err(err) => {
            warn!("could not compute review diff: {err:#}");
            return;
        }
    };

    let (verdict, reasoning) = run_reviewer(&state.review_command, &diff).await;
    state.deploy.record_review(candidate, &verdict, &reasoning);

    if verdict == "APPROVED" {
        if let Err(err) = state.deploy.deploy(candidate) {
            warn!("approved deploy failed: {err:#}");
            return;
        }
        refresh_served_apps(state);
        push::notify_all(state, "liquid ✓", "Reviewed and deployed a change").await;
    } else {
        state.deploy.mark_rejected(candidate, &reasoning);
        let msg = format!(
            "⚠️ Review rejected the latest change — it is NOT live. Reason:\n{reasoning}\n\n\
             Ask me to fix it, or approve it anyway from the pipeline banner."
        );
        let _ = state.db.append_message(origin_conversation, "scheduled", &msg);
        push::notify_all(state, "liquid ⚠️", "A change was rejected in review").await;
    }
    let _ = state.client_events.send(ServerEvent::Pipeline {
        status: state.deploy.status(),
    });
}

/// Spawn the review command, feed it the diff on stdin, parse `{verdict,
/// reasoning}` from stdout. Any failure is a conservative REJECTED so unre-
/// viewable code never auto-deploys.
async fn run_reviewer(command: &[String], diff: &str) -> (String, String) {
    use tokio::io::AsyncWriteExt;

    let (program, args) = command.split_first().expect("review command non-empty");
    let mut child = match tokio::process::Command::new(program)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => return ("REJECTED".into(), format!("could not start reviewer: {err}")),
    };
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(diff.as_bytes()).await;
        drop(stdin); // EOF
    }
    let output = match child.wait_with_output().await {
        Ok(output) => output,
        Err(err) => return ("REJECTED".into(), format!("reviewer failed: {err}")),
    };
    let stdout = String::from_utf8_lossy(&output.stdout);
    // The verdict is the last non-empty JSON line.
    let parsed = stdout
        .lines()
        .rev()
        .find_map(|line| serde_json::from_str::<serde_json::Value>(line.trim()).ok());
    match parsed {
        Some(value) => {
            let verdict = value
                .get("verdict")
                .and_then(|v| v.as_str())
                .unwrap_or("REJECTED")
                .to_uppercase();
            let reasoning = value
                .get("reasoning")
                .and_then(|v| v.as_str())
                .unwrap_or("(no reasoning provided)")
                .to_string();
            let verdict = if verdict == "APPROVED" { "APPROVED" } else { "REJECTED" };
            (verdict.to_string(), reasoning)
        }
        None => ("REJECTED".into(), "reviewer produced no parseable verdict".into()),
    }
}

/// Public shim so the pipeline-approve endpoint can refresh served apps.
pub fn refresh_served_apps_pub(state: &AppState) {
    refresh_served_apps(state);
}

/// Rescan the deployed worktree, reconcile backends, broadcast changes.
fn refresh_served_apps(state: &AppState) {
    let fresh = apps::scan_apps(&state.served_dir);
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
            apps: apps::enriched_apps(state),
        });
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
