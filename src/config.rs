use std::path::PathBuf;

use anyhow::Context;

pub const DEFAULT_PORT: u16 = 3000;
const DEFAULT_WORKSPACE_DIR: &str = "dev-workspace";
const DEFAULT_DATA_DIR: &str = "dev-data";
const DEFAULT_AGENT_CMD: &str = "bun run workspace/agent/harness.ts";
const FAKE_AGENT_CMD: &str = "bun run workspace/agent/fake-harness.ts";

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
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_WORKSPACE_DIR));

        let data_dir = std::env::var("LIQUID_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(DEFAULT_DATA_DIR));

        let raw_command = match std::env::var("LIQUID_AGENT_CMD") {
            Ok(custom) => custom,
            Err(_) if std::env::var("LIQUID_FAKE_AGENT").is_ok() => FAKE_AGENT_CMD.to_string(),
            Err(_) => DEFAULT_AGENT_CMD.to_string(),
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
