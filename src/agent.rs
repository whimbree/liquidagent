use std::process::Stdio;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::{broadcast, mpsc};
use tracing::{error, info, warn};

use crate::config::Config;

const RESPAWN_DELAY_BASE: Duration = Duration::from_secs(1);
const RESPAWN_DELAY_MAX: Duration = Duration::from_secs(30);
/// A child that dies faster than this is considered crash-looping.
const STABLE_UPTIME_THRESHOLD: Duration = Duration::from_secs(5);
const CRASH_LOOP_HINT_AFTER: u32 = 3;
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
        /// Model alias for this query (opus/sonnet/haiku). Absent = the CLI
        /// default. Read live from settings so a change takes effect next query.
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// Images the human attached to this message (base64), forwarded to the
        /// model as image content blocks. Empty for a plain text message.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<Attachment>,
    },
    Stop {
        id: String,
    },
}

/// An image the human pasted/attached into chat. `mime` is a supported image
/// type (validated at the WS boundary); `data` is base64 (no data: prefix).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Attachment {
    pub mime: String,
    pub data: String,
}

/// Image MIME types liquid accepts as chat attachments.
pub const ATTACHMENT_MIMES: [&str; 4] = ["image/png", "image/jpeg", "image/gif", "image/webp"];

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
    /// The agent wants to notify its human (push + toast).
    Notify {
        id: String,
        title: String,
        body: String,
    },
    /// An image the agent produced (e.g. its screenshot tool) to show the human
    /// in the chat. `data` is base64. Stored as an attachment + fanned to shells.
    Image {
        id: String,
        mime: String,
        data: String,
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
    pub fn start(config: &Config, internal_secret: &str) -> Self {
        let (request_tx, request_rx) = mpsc::channel(REQUEST_CHANNEL_CAPACITY);
        let (event_tx, _) = broadcast::channel(EVENT_CHANNEL_CAPACITY);

        tokio::spawn(manage_agent_process(
            config.clone(),
            internal_secret.to_string(),
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
    internal_secret: String,
    mut requests: mpsc::Receiver<AgentRequest>,
    events: broadcast::Sender<AgentEvent>,
) {
    let (program, args) = config
        .agent_command
        .split_first()
        .expect("agent command validated non-empty in Config::from_env");
    let mut fast_exits: u32 = 0;

    loop {
        let spawned_at = std::time::Instant::now();
        let mut child = match Command::new(program)
            .args(args)
            .env("LIQUID_WORKSPACE_DIR", &config.workspace_dir)
            // So the harness's screenshot tool can reach apps at /app/<id>/.
            .env("LIQUID_PORT", config.port.to_string())
            // The screenshot capability — lets the tool view PRIVATE apps. Only
            // the harness gets it; app backends (spawned elsewhere) never do.
            .env("LIQUID_INTERNAL_SECRET", &internal_secret)
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
                fast_exits = fast_exits.saturating_add(1);
                tokio::time::sleep(respawn_delay(fast_exits)).await;
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

        if spawned_at.elapsed() >= STABLE_UPTIME_THRESHOLD {
            fast_exits = 0;
        } else {
            fast_exits = fast_exits.saturating_add(1);
        }
        let delay = respawn_delay(fast_exits);
        warn!("agent harness exited ({status:?}); respawning in {delay:?}");
        if fast_exits == CRASH_LOOP_HINT_AFTER {
            error!(
                "agent harness is crash-looping. Check that the command is right \
                 (LIQUID_AGENT_CMD or the built-in default: {}), that bun is on PATH, \
                 and that its stderr above explains the exit.",
                config.agent_command.join(" ")
            );
        }
        tokio::time::sleep(delay).await;
    }
}

/// 1s, 2s, 4s, ... capped at 30s while the harness is crash-looping.
fn respawn_delay(fast_exits: u32) -> Duration {
    if fast_exits == 0 {
        return RESPAWN_DELAY_BASE;
    }
    RESPAWN_DELAY_BASE
        .saturating_mul(1u32 << fast_exits.min(5))
        .min(RESPAWN_DELAY_MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn respawn_backoff_escalates_and_caps() {
        assert_eq!(respawn_delay(0), Duration::from_secs(1));
        assert_eq!(respawn_delay(1), Duration::from_secs(2));
        assert_eq!(respawn_delay(2), Duration::from_secs(4));
        assert_eq!(respawn_delay(4), Duration::from_secs(16));
        assert_eq!(respawn_delay(5), Duration::from_secs(30)); // capped
        assert_eq!(respawn_delay(60), Duration::from_secs(30)); // no overflow
    }

    /// Rust half of the Rust↔TS wire-protocol parity guard (the TS half is
    /// workspace/agent/protocol.test.ts). Both check their own definitions
    /// against the shared workspace/agent/protocol.json, so drift on either
    /// side fails a test.
    #[test]
    fn wire_types_match_protocol_json() {
        use std::collections::BTreeSet;
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("workspace/agent/protocol.json");
        // Skip gracefully if the harness tree isn't in this build context; the
        // TS side still guards the contract there.
        let Ok(raw) = std::fs::read_to_string(&path) else {
            eprintln!("skipping wire parity: {} not found", path.display());
            return;
        };
        let contract: serde_json::Value =
            serde_json::from_str(&raw).expect("protocol.json is valid JSON");
        let contract_set = |key: &str| -> BTreeSet<String> {
            contract[key]
                .as_array()
                .expect("contract key is an array")
                .iter()
                .map(|v| v.as_str().expect("contract value is a string").to_string())
                .collect()
        };
        let type_of = |v: serde_json::Value| -> String {
            v["type"].as_str().expect("serialized variant has a type tag").to_string()
        };

        let events: BTreeSet<String> = [
            serde_json::to_value(AgentEvent::Token { id: "i".into(), text: "t".into() }).unwrap(),
            serde_json::to_value(AgentEvent::Tool { id: "i".into(), name: "n".into(), status: "start".into() }).unwrap(),
            serde_json::to_value(AgentEvent::Done { id: "i".into(), used_file_tools: false }).unwrap(),
            serde_json::to_value(AgentEvent::Error { id: "i".into(), message: "m".into() }).unwrap(),
            serde_json::to_value(AgentEvent::Session { id: "i".into(), session_id: "s".into() }).unwrap(),
            serde_json::to_value(AgentEvent::Shell { id: "i".into(), action: "open_app".into(), app: "a".into() }).unwrap(),
            serde_json::to_value(AgentEvent::Notify { id: "i".into(), title: "t".into(), body: "b".into() }).unwrap(),
            serde_json::to_value(AgentEvent::Image { id: "i".into(), mime: "image/png".into(), data: "x".into() }).unwrap(),
        ]
        .into_iter()
        .map(type_of)
        .collect();
        assert_eq!(events, contract_set("agentEventTypes"), "AgentEvent wire types drifted from protocol.json");

        let requests: BTreeSet<String> = [
            serde_json::to_value(AgentRequest::Query { id: "i".into(), prompt: "p".into(), session_id: None, model: None, attachments: vec![] }).unwrap(),
            serde_json::to_value(AgentRequest::Stop { id: "i".into() }).unwrap(),
        ]
        .into_iter()
        .map(type_of)
        .collect();
        assert_eq!(requests, contract_set("agentRequestTypes"), "AgentRequest wire types drifted from protocol.json");
    }
}
