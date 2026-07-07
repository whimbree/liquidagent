use std::path::PathBuf;

use anyhow::Context;

pub const DEFAULT_PORT: u16 = 3000;

/// Dev defaults are anchored to the repo root at compile time so `cargo run`
/// works from any directory. Deployments (the NixOS module) always set the
/// LIQUID_* env vars explicitly and never rely on these.
const REPO_ROOT: &str = env!("CARGO_MANIFEST_DIR");

#[derive(Clone, Debug)]
pub struct Config {
    pub port: u16,
    /// The directory the agent is allowed to modify. Created + `git init`ed on boot.
    pub workspace_dir: PathBuf,
    /// Platform state: SQLite database. NOT agent-writable.
    pub data_dir: PathBuf,
    /// Command used to spawn the agent harness child process.
    pub agent_command: Vec<String>,
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let port = match std::env::var("LIQUID_PORT") {
            Ok(raw) => raw
                .parse::<u16>()
                .with_context(|| format!("LIQUID_PORT is not a valid port: {raw}"))?,
            Err(_) => DEFAULT_PORT,
        };

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

        Ok(Self {
            port,
            workspace_dir,
            data_dir,
            agent_command,
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
