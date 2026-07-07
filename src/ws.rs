use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::agent::AgentRequest;
use crate::AppState;

const TITLE_MAX_CHARS: usize = 48;

/// Messages the browser sends over the WebSocket.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    UserMessage {
        content: String,
        conversation_id: Option<i64>,
    },
    Stop {
        conversation_id: i64,
    },
}

/// Events the server pushes to every connected client. Fan-out of agent
/// events (already persisted by the recorder task) plus conversation
/// lifecycle notifications.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerEvent {
    Token {
        conversation_id: i64,
        text: String,
    },
    Tool {
        conversation_id: i64,
        name: String,
        status: String,
    },
    Done {
        conversation_id: i64,
    },
    Error {
        conversation_id: i64,
        message: String,
    },
    ConversationCreated {
        conversation_id: i64,
        title: String,
    },
    /// The set of installed apps changed (agent created/edited/removed one).
    /// Payload matches /api/apps: manifests enriched with backend status.
    AppsChanged {
        apps: Vec<serde_json::Value>,
    },
    /// The agent asked the shell to do something (e.g. open an app).
    ShellCommand {
        action: String,
        app: String,
    },
    /// The agent notified its human — shells show a toast (push went out too).
    Notify {
        title: String,
        body: String,
    },
    /// The deploy pipeline's state changed (reviewing / rejected / clean).
    Pipeline {
        status: crate::deploy::PipelineStatus,
    },
    /// The single agent worker started or finished a conversation's query.
    /// Because queries serialize, this tells every client why chat may pause
    /// (e.g. a scheduled task is running).
    AgentBusy {
        conversation_id: i64,
        title: String,
        busy: bool,
    },
}

pub async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, state))
}

async fn handle_socket(mut socket: WebSocket, state: AppState) {
    info!("chat client connected");
    let mut events = state.client_events.subscribe();

    loop {
        tokio::select! {
            event = events.recv() => match event {
                Ok(event) => {
                    let Ok(payload) = serde_json::to_string(&event) else { continue };
                    if socket.send(Message::text(payload)).await.is_err() {
                        break;
                    }
                }
                Err(err) => {
                    warn!("client event stream lagged or closed: {err}");
                    break;
                }
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<ClientMessage>(&text) {
                        Ok(message) => {
                            if let Err(err) = handle_client_message(&state, message).await {
                                warn!("failed to handle client message: {err:#}");
                            }
                        }
                        Err(err) => warn!("unparseable client message: {err}: {text}"),
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {} // binary/ping/pong — axum answers pings itself
                Some(Err(err)) => {
                    warn!("websocket error: {err}");
                    break;
                }
            },
        }
    }
    info!("chat client disconnected");
}

async fn handle_client_message(state: &AppState, message: ClientMessage) -> anyhow::Result<()> {
    match message {
        ClientMessage::UserMessage {
            content,
            conversation_id,
        } => {
            let conversation_id = match conversation_id {
                Some(id) => id,
                None => {
                    let title: String = content.chars().take(TITLE_MAX_CHARS).collect();
                    let id = state.db.create_conversation(title.trim())?;
                    let _ = state.client_events.send(ServerEvent::ConversationCreated {
                        conversation_id: id,
                        title,
                    });
                    id
                }
            };
            state.db.append_message(conversation_id, "user", &content)?;
            let session_id = state.db.conversation_session(conversation_id)?;
            state
                .agent
                .send(AgentRequest::Query {
                    id: conversation_id.to_string(),
                    prompt: content,
                    session_id,
                })
                .await?;
        }
        ClientMessage::Stop { conversation_id } => {
            state
                .agent
                .send(AgentRequest::Stop {
                    id: conversation_id.to_string(),
                })
                .await?;
        }
    }
    Ok(())
}
