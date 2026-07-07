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

    axum::serve(listener, app).await.context("server error")
}

/// First-run workspace init: create the directory and `git init` it so every
/// agent change can be committed from day one.
fn init_workspace(dir: &Path) -> anyhow::Result<()> {
    if !dir.exists() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        info!("created workspace at {}", dir.display());
    }
    if !dir.join(".git").exists() {
        let status = std::process::Command::new("git")
            .arg("init")
            .arg("--initial-branch=main")
            .current_dir(dir)
            .status();
        match status {
            Ok(status) if status.success() => info!("initialized git repo in workspace"),
            Ok(status) => warn!("git init exited with {status}; workspace is not versioned"),
            Err(err) => warn!("git not available ({err}); workspace is not versioned"),
        }
    }
    Ok(())
}
