use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::Response;
use serde::Deserialize;
use tracing::{info, warn};

use crate::agent::{Agent, AgentRequest};

/// Messages the browser sends over the WebSocket.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    UserMessage { content: String },
    Stop,
}

pub async fn ws_handler(ws: WebSocketUpgrade, State(agent): State<Agent>) -> Response {
    ws.on_upgrade(move |socket| handle_socket(socket, agent))
}

async fn handle_socket(mut socket: WebSocket, agent: Agent) {
    info!("chat client connected");
    let mut events = agent.subscribe();
    let mut active_query_id: u64 = 0;

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
                    warn!("event subscription lagged or closed: {err}");
                    break;
                }
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Text(text))) => {
                    match serde_json::from_str::<ClientMessage>(&text) {
                        Ok(ClientMessage::UserMessage { content }) => {
                            active_query_id += 1;
                            let request = AgentRequest::Query {
                                id: active_query_id.to_string(),
                                prompt: content,
                            };
                            if let Err(err) = agent.send(request).await {
                                warn!("failed to forward query to agent: {err}");
                            }
                        }
                        Ok(ClientMessage::Stop) => {
                            let request = AgentRequest::Stop { id: active_query_id.to_string() };
                            if let Err(err) = agent.send(request).await {
                                warn!("failed to forward stop to agent: {err}");
                            }
                        }
                        Err(err) => warn!("unparseable client message: {err}: {text}"),
                    }
                }
                Some(Ok(Message::Close(_))) | None => break,
                Some(Ok(_)) => {} // ignore binary/ping/pong; axum answers pings itself
                Some(Err(err)) => {
                    warn!("websocket error: {err}");
                    break;
                }
            },
        }
    }
    info!("chat client disconnected");
}
