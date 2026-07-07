use axum::extract::{Path, Query, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::json;

use crate::auth;
use crate::db::Db;
use crate::AppState;

/// The model aliases the Control Panel offers. "default" (and unset) means:
/// pass nothing to the harness and let the Claude CLI / plan choose. Aliases
/// are plan-aware and version-stable, which is why we store them rather than
/// pinned model IDs.
pub const AGENT_MODEL_KEY: &str = "agent_model";
pub const AGENT_MODELS: [&str; 4] = ["default", "opus", "sonnet", "haiku"];

/// The model alias to attach to a harness query, or None to let the CLI default
/// stand. Read live so a Control-Panel change takes effect on the next query.
pub fn agent_model(db: &Db) -> Option<String> {
    match db.get_setting(AGENT_MODEL_KEY).ok().flatten() {
        Some(m) if m != "default" && AGENT_MODELS.contains(&m.as_str()) => Some(m),
        _ => None,
    }
}

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

#[derive(Deserialize)]
pub struct ChangePasswordBody {
    old_password: String,
    new_password: String,
}

/// Change the password for a logged-in owner (behind `require_auth`). Verifies
/// the current password, then rotates: every existing session is revoked (any
/// token leaked under the old password dies) and a fresh one is issued for this
/// client. A wrong current password is a 403, not a 401 — the caller's *session*
/// is fine, so the client must not treat it as a sign-out.
pub async fn auth_change_password(
    State(state): State<AppState>,
    Json(body): Json<ChangePasswordBody>,
) -> ApiResult<Response> {
    if !auth::verify_password(&state.db, &body.old_password)? {
        return Ok((
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "current password is incorrect" })),
        )
            .into_response());
    }
    if body.new_password.len() < 8 {
        return Ok((
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "new password must be at least 8 characters" })),
        )
            .into_response());
    }
    auth::set_password(&state.db, &body.new_password)?;
    state.db.clear_auth_sessions()?;
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

// --- settings endpoints -----------------------------------------------------------

pub async fn get_settings(State(state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
    let model = state
        .db
        .get_setting(AGENT_MODEL_KEY)?
        .unwrap_or_else(|| "default".to_string());
    Ok(Json(json!({ "model": model, "models": AGENT_MODELS })))
}

#[derive(Deserialize)]
pub struct SettingsBody {
    model: Option<String>,
}

pub async fn put_settings(
    State(state): State<AppState>,
    Json(body): Json<SettingsBody>,
) -> ApiResult<Response> {
    if let Some(model) = body.model {
        if !AGENT_MODELS.contains(&model.as_str()) {
            return Ok(
                (StatusCode::BAD_REQUEST, Json(json!({ "error": "unknown model" }))).into_response(),
            );
        }
        state.db.set_setting(AGENT_MODEL_KEY, &model)?;
    }
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_model_only_passes_known_non_default_aliases() {
        let db = Db::open_in_memory().unwrap();
        assert_eq!(agent_model(&db), None); // unset → let the CLI decide
        db.set_setting(AGENT_MODEL_KEY, "default").unwrap();
        assert_eq!(agent_model(&db), None); // "default" is an explicit no-op
        db.set_setting(AGENT_MODEL_KEY, "opus").unwrap();
        assert_eq!(agent_model(&db), Some("opus".to_string()));
        db.set_setting(AGENT_MODEL_KEY, "gpt-4").unwrap();
        assert_eq!(agent_model(&db), None); // outside the allowlist
    }
}
