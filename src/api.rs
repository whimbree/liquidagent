use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::auth;
use crate::AppState;

type ApiResult<T> = Result<T, ApiError>;

pub struct ApiError(anyhow::Error);

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        tracing::warn!("api error: {:#}", self.0);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": self.0.to_string() })),
        )
            .into_response()
    }
}

// --- auth endpoints (exempt from the auth middleware) --------------------------

#[derive(Deserialize)]
pub struct PasswordBody {
    password: String,
}

pub async fn auth_status(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let set = auth::password_is_set(&state.db)?;
    Ok(Json(json!({ "password_set": set })))
}

/// One-time setup: only works while no password exists.
pub async fn auth_setup(
    State(state): State<AppState>,
    Json(body): Json<PasswordBody>,
) -> ApiResult<Response> {
    if auth::password_is_set(&state.db)? {
        return Ok((StatusCode::CONFLICT, Json(json!({ "error": "already set up" }))).into_response());
    }
    auth::set_password(&state.db, &body.password)?;
    let token = auth::create_session(&state.db)?;
    Ok(Json(json!({ "token": token })).into_response())
}

pub async fn auth_login(
    State(state): State<AppState>,
    Json(body): Json<PasswordBody>,
) -> ApiResult<Response> {
    if !auth::verify_password(&state.db, &body.password)? {
        return Ok((
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "wrong password" })),
        )
            .into_response());
    }
    let token = auth::create_session(&state.db)?;
    Ok(Json(json!({ "token": token })).into_response())
}

// --- auth middleware ------------------------------------------------------------

#[derive(Deserialize)]
pub struct TokenQuery {
    token: Option<String>,
}

/// Guards /api/* data routes and /ws. Accepts `Authorization: Bearer <token>`
/// or `?token=` (WebSocket connects can't set headers from a browser).
pub async fn require_auth(
    State(state): State<AppState>,
    Query(query): Query<TokenQuery>,
    request: Request,
    next: Next,
) -> Response {
    let header_token = request
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let token = header_token.or(query.token.as_deref());
    match token {
        Some(token) if auth::validate_token(&state.db, token) => next.run(request).await,
        _ => (
            StatusCode::UNAUTHORIZED,
            Json(json!({ "error": "unauthorized" })),
        )
            .into_response(),
    }
}

// --- conversation endpoints -------------------------------------------------------

pub async fn list_conversations(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let conversations = state.db.list_conversations()?;
    Ok(Json(json!({ "conversations": conversations })))
}

pub async fn list_messages(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    let messages = state.db.list_messages(id)?;
    Ok(Json(json!({ "messages": messages })))
}

pub async fn delete_conversation(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> ApiResult<StatusCode> {
    state.db.delete_conversation(id)?;
    Ok(StatusCode::NO_CONTENT)
}

// --- pipeline endpoints -----------------------------------------------------------

pub async fn pipeline_status(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({
        "mode": state.deploy.mode(),
        "status": state.deploy.status(),
    }))
}

#[derive(Deserialize)]
pub struct ModeBody {
    mode: String,
}

pub async fn set_pipeline_mode(
    State(state): State<AppState>,
    Json(body): Json<ModeBody>,
) -> ApiResult<StatusCode> {
    let Some(mode) = crate::deploy::PipelineMode::parse(&body.mode) else {
        return Ok(StatusCode::BAD_REQUEST);
    };
    state.deploy.set_mode(mode);
    state.deploy.persist_mode(&state.db);
    Ok(StatusCode::NO_CONTENT)
}

/// Human override: deploy the currently-rejected candidate anyway.
pub async fn pipeline_approve(State(state): State<AppState>) -> ApiResult<StatusCode> {
    let candidate = match state.deploy.status() {
        crate::deploy::PipelineStatus::Rejected { candidate, .. } => candidate,
        _ => return Ok(StatusCode::CONFLICT),
    };
    state.deploy.record_review(&candidate, "APPROVED (human override)", "Deployed by the owner.");
    state.deploy.deploy(&candidate)?;
    crate::refresh_served_apps_pub(&state);
    let _ = state.client_events.send(crate::ws::ServerEvent::Pipeline {
        status: state.deploy.status(),
    });
    Ok(StatusCode::NO_CONTENT)
}
