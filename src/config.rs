use std::net::{IpAddr, Ipv4Addr};
use std::path::PathBuf;

use anyhow::Context;

pub const DEFAULT_PORT: u16 = 3000;
/// Bind localhost by default: Phase 0/1 assumes a reverse proxy (with SSO or at
/// least TLS) fronts the supervisor. Deployments that sit behind such a proxy
/// on another host (e.g. a microVM reached from a gateway VM) set `LIQUID_HOST`.
pub const DEFAULT_HOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// Dev defaults are anchored to the repo root at compile time so `cargo run`
/// works from any directory. Deployments (the NixOS module) always set the
/// LIQUID_* env vars explicitly and never rely on these.
const REPO_ROOT: &str = env!("CARGO_MANIFEST_DIR");

#[derive(Clone, Debug)]
pub struct Config {
    pub port: u16,
    /// Address the supervisor binds. Defaults to localhost; set `LIQUID_HOST`
    /// (e.g. `0.0.0.0`) only when a trusted reverse proxy fronts it.
    pub host: IpAddr,
    /// If set and no password is configured yet, seed this as the initial login
    /// password on boot (`LIQUID_INITIAL_PASSWORD`). Meant for reproducible
    /// deploys; the user should change it after first login.
    pub initial_password: Option<String>,
    /// The directory the agent is allowed to modify. Created + `git init`ed on boot.
    pub workspace_dir: PathBuf,
    /// Platform state: SQLite database. NOT agent-writable.
    pub data_dir: PathBuf,
    /// Command used to spawn the agent harness child process.
    pub agent_command: Vec<String>,
    /// Command used to run a one-shot code review (diff on stdin, verdict JSON
    /// on stdout).
    pub review_command: Vec<String>,
    /// Default deploy pipeline mode (overridden by a saved DB setting).
    pub pipeline_mode: crate::deploy::PipelineMode,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let port = match std::env::var("LIQUID_PORT") {
            Ok(raw) => raw
                .parse::<u16>()
                .with_context(|| format!("LIQUID_PORT is not a valid port: {raw}"))?,
            Err(_) => DEFAULT_PORT,
        };

        let host = match std::env::var("LIQUID_HOST") {
            Ok(raw) => raw
                .parse::<IpAddr>()
                .with_context(|| format!("LIQUID_HOST is not a valid IP address: {raw}"))?,
            Err(_) => DEFAULT_HOST,
        };

        // Empty is treated as unset so an unpopulated EnvironmentFile line is harmless.
        let initial_password = std::env::var("LIQUID_INITIAL_PASSWORD")
            .ok()
            .filter(|s| !s.is_empty());

        let workspace_dir = std::env::var("LIQUID_WORKSPACE_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(REPO_ROOT).join("dev-workspace"));

        let data_dir = std::env::var("LIQUID_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(REPO_ROOT).join("dev-data"));

        let raw_command = match std::env::var("LIQUID_AGENT_CMD") {
            Ok(custom) => custom,
            Err(_) if std::env::var("LIQUID_FAKE_AGENT").is_ok() => {
                format!("bun run {REPO_ROOT}/workspace/agent/fake-harness.ts")
            }
            Err(_) => format!("bun run {REPO_ROOT}/workspace/agent/harness.ts"),
        };
        let agent_command: Vec<String> = raw_command.split_whitespace().map(String::from).collect();
        anyhow::ensure!(!agent_command.is_empty(), "agent command must not be empty");

        let raw_review = match std::env::var("LIQUID_REVIEW_CMD") {
            Ok(custom) => custom,
            Err(_) if std::env::var("LIQUID_FAKE_AGENT").is_ok() => {
                format!("bun run {REPO_ROOT}/workspace/agent/fake-review.ts")
            }
            Err(_) => format!("bun run {REPO_ROOT}/workspace/agent/review.ts"),
        };
        let review_command: Vec<String> = raw_review.split_whitespace().map(String::from).collect();
        anyhow::ensure!(!review_command.is_empty(), "review command must not be empty");

        let pipeline_mode = std::env::var("LIQUID_PIPELINE_MODE")
            .ok()
            .and_then(|s| crate::deploy::PipelineMode::parse(&s))
            .unwrap_or(crate::deploy::PipelineMode::Vibe);

        Ok(Self {
            port,
            host,
            initial_password,
            workspace_dir,
            data_dir,
            agent_command,
            review_command,
            pipeline_mode,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Dev defaults must be absolute so `cargo run` works from any cwd.
    /// (Assumes the LIQUID_* env vars aren't set in the test environment.)
    #[test]
    fn dev_defaults_are_cwd_independent() {
        if std::env::var("LIQUID_WORKSPACE_DIR").is_ok() || std::env::var("LIQUID_AGENT_CMD").is_ok()
        {
            return; // environment overrides in play; nothing to assert
        }
        let config = Config::from_env().unwrap();
        assert!(config.workspace_dir.is_absolute());
        assert!(config.data_dir.is_absolute());
        let harness = config.agent_command.last().unwrap();
        assert!(std::path::Path::new(harness).is_absolute());
        assert!(std::path::Path::new(harness).exists(), "harness file should exist in repo");
    }
}
