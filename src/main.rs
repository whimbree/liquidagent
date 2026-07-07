mod agent;
mod config;
mod ws;

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::Path;

use anyhow::Context;
use axum::response::Html;
use axum::routing::get;
use axum::{Json, Router};
use tracing::{info, warn};

use crate::agent::Agent;
use crate::config::Config;

/// The chat page is embedded at compile time — no runtime path discovery.
const CHAT_PAGE: &str = include_str!("../static/chat.html");

/// Workspace template, embedded so first-run init needs no runtime paths.
const WORKSPACE_TEMPLATE: &[(&str, &str)] = &[
    ("MYSELF.md", include_str!("../default-workspace/MYSELF.md")),
    ("MYHUMAN.md", include_str!("../default-workspace/MYHUMAN.md")),
    ("MEMORY.md", include_str!("../default-workspace/MEMORY.md")),
];

// Phase 0 binds localhost only; your reverse proxy (with SSO) fronts it.
const BIND_ADDR: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

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

    let agent = Agent::start(&config);

    let app = Router::new()
        .route("/", get(|| async { Html(CHAT_PAGE) }))
        .route(
            "/api/health",
            get(|| async { Json(serde_json::json!({ "status": "ok" })) }),
        )
        .route("/ws", get(ws::ws_handler))
        .with_state(agent);

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
