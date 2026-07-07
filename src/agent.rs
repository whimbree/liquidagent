use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};

use crate::config::Config;

const AGENT_RESPAWN_DELAY: Duration = Duration::from_secs(1);
const EVENT_CHANNEL_CAPACITY: usize = 1024;
const REQUEST_CHANNEL_CAPACITY: usize = 64;

/// Requests sent to the agent harness over stdin (one JSON object per line).
/// `id` is the conversation id; `session_id` resumes that conversation's
/// Claude session when present.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentRequest {
    Query {
        id: String,
        prompt: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
    },
    Stop {
        id: String,
    },
}

/// Events received from the agent harness over stdout (one JSON object per line).
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    Token {
        id: String,
        text: String,
    },
    Tool {
        id: String,
        name: String,
        status: String,
    },
    Done {
        id: String,
        used_file_tools: bool,
    },
    Error {
        id: String,
        message: String,
    },
    /// The harness reports the Claude session backing this conversation so
    /// the supervisor can persist it for resume.
    Session {
        id: String,
        session_id: String,
    },
    /// The agent drove the shell (via its liquid-shell MCP tool).
    Shell {
        id: String,
        action: String,
        app: String,
    },
}

/// Handle to the agent harness child process. Cloneable; all clones talk to
/// the same child. The manager task respawns the child if it exits.
#[derive(Clone)]
pub struct Agent {
    requests: mpsc::Sender<AgentRequest>,
    events: broadcast::Sender<AgentEvent>,
}

impl Agent {
    pub fn start(config: &Config) -> Self {
        let (request_tx, request_rx) = mpsc::channel(REQUEST_CHANNEL_CAPACITY);
        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);

        tokio::spawn(manage_agent_process(
            config.clone(),
            request_rx,
            event_tx.clone(),
        ));

        Self {
            requests: request_tx,
            events: event_tx,
        }
    }

    pub async fn send(&self, request: AgentRequest) -> anyhow::Result<()> {
        self.requests
            .send(request)
            .await
            .map_err(|_| anyhow::anyhow!("agent manager task is gone"))
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AgentEvent> {
        self.events.subscribe()
    }
}

/// Owns the child process. Forwards queued requests to its stdin and parsed
/// stdout lines to the event channel. Respawns the child on exit; requests
/// sent while the child is down wait in the channel until it's back.
async fn manage_agent_process(
    config: Config,
    mut requests: mpsc::Receiver<AgentRequest>,
    events: broadcast::Sender<AgentEvent>,
) {
    let (program, args) = config
        .agent_command
        .split_first()
        .expect("agent command validated non-empty in Config::from_env");

    loop {
        let mut child = match Command::new(program)
            .args(args)
            .env("LIQUID_WORKSPACE_DIR", &config.workspace_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            // The harness must never outlive the supervisor.
            .kill_on_drop(true)
            .spawn()
        {
            Ok(child) => child,
            Err(err) => {
                error!("failed to spawn agent harness ({program}): {err}");
                tokio::time::sleep(AGENT_RESPAWN_DELAY).await;
                continue;
            }
        };
        info!("agent harness spawned: {}", config.agent_command.join(" "));

        let mut stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let mut lines = BufReader::new(stdout).lines();

        loop {
            tokio::select! {
                line = lines.next_line() => match line {
                    Ok(Some(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<AgentEvent>(&line) {
                            // No subscribers connected is fine; drop the event.
                            Ok(event) => { let _ = events.send(event); }
                            Err(err) => warn!("unparseable agent event: {err}: {line}"),
                        }
                    }
                    Ok(None) => {
                        warn!("agent harness stdout closed");
                        break;
                    }
                    Err(err) => {
                        error!("error reading agent stdout: {err}");
                        break;
                    }
                },
                request = requests.recv() => match request {
                    Some(request) => {
                        let mut payload = match serde_json::to_string(&request) {
                            Ok(payload) => payload,
                            Err(err) => {
                                error!("failed to serialize agent request: {err}");
                                continue;
                            }
                        };
                        payload.push('\n');
                        if let Err(err) = stdin.write_all(payload.as_bytes()).await {
                            error!("failed to write to agent stdin: {err}");
                            break;
                        }
                    }
                    None => {
                        info!("request channel closed; shutting down agent");
                        let _ = child.kill().await;
                        return;
                    }
                },
            }
        }

        let _ = child.kill().await;
        let status = child.wait().await;
        warn!("agent harness exited ({status:?}); respawning in {AGENT_RESPAWN_DELAY:?}");
        tokio::time::sleep(AGENT_RESPAWN_DELAY).await;
    }
}
