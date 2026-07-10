use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

use axum::extract::{Path as UrlPath, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

use crate::AppState;

const APPS_DIR: &str = "apps";
const MANIFEST_FILE: &str = "app.json";
const DEFAULT_ICON: &str = "📦";

/// Optional window geometry an app can declare (camelCase in JSON). Absent
/// fields fall back to the shell's defaults; passed through to the shell so a
/// calculator opens small and a dashboard opens wide.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AppWindow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_height: Option<u32>,
}

/// Who may reach an app's surfaces (static files + backend proxy). Private is
/// the default: only a logged-in owner (session cookie) or loopback (the
/// supervisor's own tooling — e.g. the agent's screenshot chromium) gets in.
/// Public apps opt in to guests: anyone who reaches the host, no liquid login.
#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Visibility {
    Public,
    #[default]
    Private,
}

/// How the app occupies its surface. `panel` (the default) is today's model:
/// a static `index.html` served by the supervisor, with an optional backend
/// behind `/app/<id>/api/*`. `full` means the backend owns the whole document —
/// every request under `/app/<id>/` (HTML, assets, sockets, any method) is
/// proxied to it, and no static files are served. (ADR 0003)
#[derive(Clone, Copy, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Surface {
    #[default]
    Panel,
    Full,
}

/// A declared backend runner (ADR 0002): how to start the app's server.
/// `run` is an argv vector (never a shell string), spawned in the app's
/// directory with `PORT`, `LIQUID_APP_ID`, and `LIQUID_APP_DATA_DIR` injected.
/// `health` is an optional HTTP readiness path ("/health"); without it,
/// readiness is a plain TCP connect. `env` is extra environment (e.g.
/// MIX_ENV=prod) — additive, never overriding the injected variables.
#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackendSpec {
    pub run: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub health: Option<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
}

/// What the agent writes to apps/<id>/app.json.
#[derive(Debug, Deserialize)]
struct RawManifest {
    name: String,
    #[serde(default)]
    icon: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    window: Option<AppWindow>,
    #[serde(default)]
    visibility: Visibility,
    #[serde(default)]
    surface: Surface,
    #[serde(default)]
    backend: Option<BackendSpec>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct AppManifest {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub description: String,
    pub has_backend: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub window: Option<AppWindow>,
    pub visibility: Visibility,
    pub surface: Surface,
    /// The normalized runner: the declared `backend` block, or the synthesized
    /// Bun default when `backend/index.ts` exists. Not sent to clients — the
    /// `backend` key in the wire shape is the *status*, injected by
    /// `enriched_apps`.
    #[serde(skip_serializing)]
    pub backend: Option<BackendSpec>,
}

/// The legacy zero-config contract: `backend/index.ts` present → run it with
/// Bun. Declared `backend` blocks in app.json take precedence.
pub const BUN_BACKEND_ENTRY: &str = "backend/index.ts";

fn default_bun_spec() -> BackendSpec {
    BackendSpec {
        run: vec!["bun".into(), "run".into(), BUN_BACKEND_ENTRY.into()],
        health: None,
        env: HashMap::new(),
    }
}

/// Validate a declared backend block, or synthesize the Bun default. A bad
/// declaration is an error (the app is skipped with a warning — same policy
/// as a malformed manifest): silently running the wrong thing is worse.
fn resolve_backend(
    declared: Option<BackendSpec>,
    dir: &Path,
) -> Result<Option<BackendSpec>, String> {
    match declared {
        Some(spec) => {
            if spec.run.is_empty() || spec.run[0].trim().is_empty() {
                return Err("backend.run must be a non-empty argv array".into());
            }
            if let Some(health) = &spec.health {
                if !health.starts_with('/') {
                    return Err(format!("backend.health must start with '/', got {health:?}"));
                }
            }
            Ok(Some(spec))
        }
        None if dir.join(BUN_BACKEND_ENTRY).exists() => Ok(Some(default_bun_spec())),
        None => Ok(None),
    }
}

/// The access rule for an app surface. Pure so it's exhaustively testable:
/// public is open; private needs an authorized caller (owner session, or the
/// supervisor's own screenshot capability). Loopback is deliberately NOT trusted
/// — app backends run on loopback too, so a guest-reachable public backend must
/// not become a read-proxy for private apps.
pub fn app_access_allowed(visibility: Visibility, is_authorized: bool) -> bool {
    match visibility {
        Visibility::Public => true,
        Visibility::Private => is_authorized,
    }
}

/// The query/cookie name carrying the per-boot screenshot capability. The agent's
/// screenshot tool (in-VM chromium) presents `?__lshot=<secret>` on the initial
/// navigation; `serve` echoes it as a Path-scoped HttpOnly cookie so the app's
/// subresource requests carry it too. Only the harness holds the secret (passed
/// at spawn) — app backends never receive it, so they can't forge it.
pub const SHOT_QUERY: &str = "__lshot";
pub const SHOT_COOKIE: &str = "liquid_shot";

/// Whether the initial navigation presents the screenshot secret in its query.
fn shot_in_query(secret: &str, raw_query: Option<&str>) -> bool {
    !secret.is_empty()
        && raw_query
            .map(|q| q.split('&').any(|kv| kv == format!("{SHOT_QUERY}={secret}")))
            .unwrap_or(false)
}

/// True if the request carries the valid screenshot capability (query or cookie).
fn shot_capability(state: &AppState, headers: &axum::http::HeaderMap, raw_query: Option<&str>) -> bool {
    let secret = state.internal_secret.as_str();
    if secret.is_empty() {
        return false;
    }
    if shot_in_query(secret, raw_query) {
        return true;
    }
    headers
        .get(axum::http::header::COOKIE)
        .and_then(|c| c.to_str().ok())
        .map(|cookies| {
            cookies
                .split(';')
                .filter_map(|c| c.trim().strip_prefix(&format!("{SHOT_COOKIE}=")))
                .any(|v| v == secret)
        })
        .unwrap_or(false)
}

/// Visibility of an app by id, from the served-apps cache. Unknown apps are
/// treated as private (deny by default; the file read 404s anyway).
pub fn app_visibility(state: &AppState, app: &str) -> Visibility {
    state
        .apps_cache
        .lock()
        .expect("apps cache poisoned")
        .iter()
        .find(|m| m.id == app)
        .map(|m| m.visibility)
        .unwrap_or(Visibility::Private)
}

/// Surface of an app by id, from the served-apps cache. Unknown apps fall
/// back to panel (static serving, which 404s for them anyway).
pub fn app_surface(state: &AppState, app: &str) -> Surface {
    state
        .apps_cache
        .lock()
        .expect("apps cache poisoned")
        .iter()
        .find(|m| m.id == app)
        .map(|m| m.surface)
        .unwrap_or_default()
}

/// Enforce the access rule for a request to `/app/<id>/*`. Returns an error
/// response to send, or None when allowed. `raw_query` is the request's query
/// string (for the screenshot capability).
pub fn check_app_access(
    state: &AppState,
    app: &str,
    headers: &axum::http::HeaderMap,
    raw_query: Option<&str>,
) -> Option<Response> {
    let vis = app_visibility(state, app);
    let is_owner = crate::auth::cookie_session_role(&state.db, headers).as_deref() == Some("owner");
    let authorized = is_owner || shot_capability(state, headers, raw_query);
    if app_access_allowed(vis, authorized) {
        None
    } else {
        Some((StatusCode::UNAUTHORIZED, "sign in to liquid to open this app").into_response())
    }
}

/// If the request presents the screenshot secret in its query, the Set-Cookie
/// value binding it path-scoped so the app's subresources carry it (chromium
/// can't set headers/cookies itself; the initial navigation bootstraps it).
pub fn shot_cookie_for(state: &AppState, app: &str, raw_query: Option<&str>) -> Option<String> {
    let secret = state.internal_secret.as_str();
    if shot_in_query(secret, raw_query) {
        Some(format!(
            "{SHOT_COOKIE}={secret}; Path=/app/{app}/; HttpOnly; SameSite=Lax; Max-Age=120"
        ))
    } else {
        None
    }
}

/// Scan workspace/apps/*/app.json. Malformed manifests are skipped with a
/// warning — a broken app must never take the shell down.
pub fn scan_apps(workspace_dir: &Path) -> Vec<AppManifest> {
    let apps_dir = workspace_dir.join(APPS_DIR);
    let Ok(entries) = std::fs::read_dir(&apps_dir) else {
        return Vec::new();
    };
    let mut apps: Vec<AppManifest> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            if !entry.file_type().ok()?.is_dir() {
                return None;
            }
            let id = entry.file_name().to_str()?.to_string();
            if !is_safe_app_id(&id) {
                return None;
            }
            let dir = entry.path();
            let manifest_path = dir.join(MANIFEST_FILE);
            let raw = std::fs::read_to_string(&manifest_path).ok()?;
            let manifest = match serde_json::from_str::<RawManifest>(&raw) {
                Ok(manifest) => manifest,
                Err(err) => {
                    warn!("skipping app {id}: bad {MANIFEST_FILE}: {err}");
                    return None;
                }
            };
            let backend = match resolve_backend(manifest.backend, &dir) {
                Ok(backend) => backend,
                Err(why) => {
                    warn!("skipping app {id}: {why}");
                    return None;
                }
            };
            // A panel app IS its static index.html; a full-surface app IS its
            // backend. An app with neither has nothing to serve.
            match manifest.surface {
                Surface::Panel if !dir.join("index.html").exists() => return None,
                Surface::Full if backend.is_none() => {
                    warn!("skipping app {id}: surface \"full\" requires a backend");
                    return None;
                }
                _ => {}
            }
            Some(AppManifest {
                name: manifest.name,
                icon: manifest.icon.unwrap_or_else(|| DEFAULT_ICON.to_string()),
                description: manifest.description.unwrap_or_default(),
                has_backend: backend.is_some(),
                window: manifest.window,
                visibility: manifest.visibility,
                surface: manifest.surface,
                backend,
                id,
            })
        })
        .collect();
    apps.sort_by(|a, b| a.id.cmp(&b.id));
    apps
}

fn is_safe_app_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

// --- endpoints -------------------------------------------------------------------

pub async fn list_apps(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({ "apps": enriched_apps(&state) }))
}

/// Manifests + live backend status, as sent to clients (REST and WS both).
pub fn enriched_apps(state: &AppState) -> Vec<serde_json::Value> {
    let apps = scan_apps(&state.served_dir);
    *state.apps_cache.lock().expect("apps cache poisoned") = apps.clone();
    apps.into_iter()
        .map(|app| {
            let backend = app.has_backend.then(|| state.backends.status(&app.id)).flatten();
            let mut value = serde_json::to_value(&app).expect("manifest serializes");
            value["backend"] = serde_json::to_value(backend).expect("status serializes");
            value
        })
        .collect()
}

/// /app/{app}/{*path} — the app's surface. Panel apps (the default) are
/// static files from the app's directory, GET/HEAD only, traversal-safe (app
/// id is charset-checked, every path component must be a plain name). Full-
/// surface apps own their whole document: every path and method is proxied to
/// the app's backend, WebSocket upgrades included.
pub async fn serve_app_file(
    State(state): State<AppState>,
    UrlPath((app, path)): UrlPath<(String, String)>,
    request: axum::extract::Request,
) -> Response {
    serve_or_proxy(state, app, path, request).await
}

pub async fn serve_app_index(
    State(state): State<AppState>,
    UrlPath(app): UrlPath<String>,
    request: axum::extract::Request,
) -> Response {
    serve_or_proxy(state, app, String::new(), request).await
}

async fn serve_or_proxy(
    state: AppState,
    app: String,
    path: String,
    request: axum::extract::Request,
) -> Response {
    let query = request.uri().query().map(str::to_string);
    if let Some(denied) = check_app_access(&state, &app, request.headers(), query.as_deref()) {
        return denied;
    }
    match app_surface(&state, &app) {
        Surface::Full => {
            // The backend serves the document; the screenshot capability still
            // needs its path-scoped cookie bootstrapped onto the response so
            // chromium's subresource requests are authorized.
            let shot_cookie = shot_cookie_for(&state, &app, query.as_deref());
            let mut response = crate::backends::proxy(state, app, path, request).await;
            if let Some(cookie) = shot_cookie.and_then(|c| header::HeaderValue::from_str(&c).ok()) {
                response.headers_mut().append(header::SET_COOKIE, cookie);
            }
            response
        }
        Surface::Panel => {
            if !matches!(*request.method(), axum::http::Method::GET | axum::http::Method::HEAD) {
                return StatusCode::METHOD_NOT_ALLOWED.into_response();
            }
            serve(state, app, path, query).await
        }
    }
}

async fn serve(state: AppState, app: String, path: String, query: Option<String>) -> Response {
    let shot_cookie = shot_cookie_for(&state, &app, query.as_deref());
    if !is_safe_app_id(&app) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let relative = if path.is_empty() { "index.html" } else { path.as_str() };
    let Some(safe_relative) = sanitize_relative(relative) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Served from the deployed worktree, not the live workspace.
    let full = state
        .served_dir
        .join(APPS_DIR)
        .join(&app)
        .join(&safe_relative);
    match tokio::fs::read(&full).await {
        Ok(bytes) => {
            let mime = mime_for(&safe_relative);
            // Agent edits should show up on refresh — never cache app files.
            let mut resp = (
                [
                    (header::CONTENT_TYPE, mime),
                    (header::CACHE_CONTROL, "no-cache"),
                ],
                bytes,
            )
                .into_response();
            // Bootstrap the screenshot cookie so chromium's asset requests carry it.
            if let Some(cookie) = shot_cookie.and_then(|c| header::HeaderValue::from_str(&c).ok()) {
                resp.headers_mut().append(header::SET_COOKIE, cookie);
            }
            resp
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Every component must be a normal name (no `..`, no absolute, no prefix).
fn sanitize_relative(path: &str) -> Option<PathBuf> {
    let candidate = Path::new(path);
    let mut clean = PathBuf::new();
    for component in candidate.components() {
        match component {
            Component::Normal(part) => clean.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if clean.as_os_str().is_empty() {
        None
    } else {
        Some(clean)
    }
}

fn mime_for(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js" | "mjs") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("json") => "application/json",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("webp") => "image/webp",
        Some("txt" | "md") => "text/plain; charset=utf-8",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// GET /api/apps/{app}/log — the app's git history within the workspace repo.
pub async fn app_log(
    State(state): State<AppState>,
    UrlPath(app): UrlPath<String>,
) -> Response {
    if !is_safe_app_id(&app) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let output = tokio::process::Command::new("git")
        .args([
            "log",
            "--format=%h%x09%ct%x09%s",
            "--max-count=30",
            "--",
            &format!("{APPS_DIR}/{app}"),
        ])
        .current_dir(&state.workspace_dir)
        .output()
        .await;
    match output {
        Ok(out) if out.status.success() => {
            let commits: Vec<serde_json::Value> = String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|line| {
                    let mut parts = line.splitn(3, '\t');
                    Some(json!({
                        "hash": parts.next()?,
                        "timestamp": parts.next()?.parse::<i64>().ok()?,
                        "subject": parts.next()?,
                    }))
                })
                .collect();
            Json(json!({ "commits": commits })).into_response()
        }
        Ok(out) => {
            warn!("git log failed: {}", String::from_utf8_lossy(&out.stderr));
            Json(json!({ "commits": [] })).into_response()
        }
        Err(err) => {
            warn!("git log spawn failed: {err:#}");
            Json(json!({ "commits": [] })).into_response()
        }
    }
}

/// POST /api/apps/{app}/graduate — carve the app out into a standalone repo.
///
/// Splits `apps/<app>`'s history into its own root (preserving the commit
/// trail — the audit story matters) and pushes it to the user-provided
/// remote as `main`. A platform operation, not an agent one: history surgery
/// stays in the trusted supervisor. The workspace copy is left in place; the
/// human decides whether to remove it afterward (via the agent).
#[derive(Deserialize)]
pub struct GraduateBody {
    remote: String,
}

pub async fn graduate(
    State(state): State<AppState>,
    UrlPath(app): UrlPath<String>,
    Json(body): Json<GraduateBody>,
) -> Response {
    if !is_safe_app_id(&app) {
        return StatusCode::NOT_FOUND.into_response();
    }
    if !state.workspace_dir.join(APPS_DIR).join(&app).exists() {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "no such app" }))).into_response();
    }
    let remote = body.remote.trim().to_string();
    if remote.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(json!({ "error": "remote is required" }))).into_response();
    }

    // 1. subtree split -> a commit whose root is the app's directory.
    let split = tokio::process::Command::new("git")
        .args(["subtree", "split", &format!("--prefix={APPS_DIR}/{app}"), "HEAD"])
        .current_dir(&state.workspace_dir)
        .output()
        .await;
    let sha = match split {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout).trim().to_string(),
        Ok(out) => {
            warn!("subtree split failed: {}", String::from_utf8_lossy(&out.stderr));
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "could not split app history" })),
            )
                .into_response();
        }
        Err(err) => {
            warn!("subtree split spawn failed: {err:#}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    // 2. push that commit to the remote as main.
    let push = tokio::process::Command::new("git")
        .args(["push", &remote, &format!("{sha}:refs/heads/main")])
        .current_dir(&state.workspace_dir)
        .output()
        .await;
    match push {
        Ok(out) if out.status.success() => {
            info!("graduated {app} -> {remote}");
            Json(json!({ "remote": remote, "ref": "main", "commit": sha })).into_response()
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
            warn!("graduate push failed: {stderr}");
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({ "error": format!("push failed: {stderr}") })),
            )
                .into_response()
        }
        Err(err) => {
            warn!("graduate push spawn failed: {err:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// --- KV endpoints -------------------------------------------------------------------

#[derive(Deserialize)]
pub struct KvBody {
    value: String,
}

pub async fn kv_get(
    State(state): State<AppState>,
    UrlPath((app, key)): UrlPath<(String, String)>,
) -> Response {
    match state.db.kv_get(&app, &key) {
        Ok(Some(value)) => Json(json!({ "value": value })).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(err) => {
            warn!("kv_get failed: {err:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn kv_put(
    State(state): State<AppState>,
    UrlPath((app, key)): UrlPath<(String, String)>,
    Json(body): Json<KvBody>,
) -> Response {
    match state.db.kv_set(&app, &key, &body.value) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            warn!("kv_put failed: {err:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn kv_delete(
    State(state): State<AppState>,
    UrlPath((app, key)): UrlPath<(String, String)>,
) -> Response {
    match state.db.kv_delete(&app, &key) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            warn!("kv_delete failed: {err:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

pub async fn kv_list(State(state): State<AppState>, UrlPath(app): UrlPath<String>) -> Response {
    match state.db.kv_list(&app) {
        Ok(keys) => Json(json!({ "keys": keys })).into_response(),
        Err(err) => {
            warn!("kv_list failed: {err:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

// --- shell layout (workspace/SHELL.json — agent-writable by design) ---------------

const SHELL_FILE: &str = "SHELL.json";

pub async fn shell_get(State(state): State<AppState>) -> Response {
    match tokio::fs::read_to_string(state.workspace_dir.join(SHELL_FILE)).await {
        Ok(contents) => match serde_json::from_str::<serde_json::Value>(&contents) {
            Ok(value) => Json(value).into_response(),
            Err(_) => Json(json!({})).into_response(),
        },
        Err(_) => Json(json!({})).into_response(),
    }
}

pub async fn shell_put(
    State(state): State<AppState>,
    Json(layout): Json<serde_json::Value>,
) -> Response {
    let pretty = serde_json::to_string_pretty(&layout).unwrap_or_else(|_| "{}".to_string());
    match tokio::fs::write(state.workspace_dir.join(SHELL_FILE), pretty).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            warn!("failed to write {SHELL_FILE}: {err:#}");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_ids_are_charset_checked() {
        assert!(is_safe_app_id("calculator"));
        assert!(is_safe_app_id("my-app_2"));
        assert!(!is_safe_app_id(""));
        assert!(!is_safe_app_id("../evil"));
        assert!(!is_safe_app_id("a/b"));
        assert!(!is_safe_app_id("a b"));
        assert!(!is_safe_app_id("a.b"));
    }

    #[test]
    fn sanitize_rejects_traversal() {
        assert_eq!(sanitize_relative("index.html"), Some(PathBuf::from("index.html")));
        assert_eq!(sanitize_relative("css/app.css"), Some(PathBuf::from("css/app.css")));
        assert_eq!(sanitize_relative("./a.js"), Some(PathBuf::from("a.js")));
        assert_eq!(sanitize_relative("../MYHUMAN.md"), None);
        assert_eq!(sanitize_relative("a/../../b"), None);
        assert_eq!(sanitize_relative("/etc/passwd"), None);
        assert_eq!(sanitize_relative(""), None);
        assert_eq!(sanitize_relative("."), None);
    }

    #[test]
    fn scan_reads_manifests_and_skips_broken_ones() {
        let root = std::env::temp_dir().join(format!("liquid-scan-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let make = |id: &str, manifest: Option<&str>, index: bool| {
            let dir = root.join("apps").join(id);
            std::fs::create_dir_all(&dir).unwrap();
            if let Some(m) = manifest {
                std::fs::write(dir.join("app.json"), m).unwrap();
            }
            if index {
                std::fs::write(dir.join("index.html"), "<h1>x</h1>").unwrap();
            }
        };
        make("good", Some(r#"{"name":"Good","icon":"✅","description":"d"}"#), true);
        make("noicon", Some(r#"{"name":"NoIcon"}"#), true);
        make("broken", Some("{not json"), true);
        make("noindex", Some(r#"{"name":"NoIndex"}"#), false);
        make("nomanifest", None, true);

        let apps = scan_apps(&root);
        let ids: Vec<&str> = apps.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, vec!["good", "noicon"]);
        assert_eq!(apps[0].icon, "✅");
        assert_eq!(apps[1].icon, DEFAULT_ICON);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn scan_of_missing_dir_is_empty() {
        assert!(scan_apps(Path::new("/nonexistent/liquid-test")).is_empty());
    }

    #[test]
    fn scan_parses_declared_window_geometry() {
        let root = std::env::temp_dir().join(format!("liquid-win-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("apps").join("calc");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("app.json"),
            r#"{"name":"Calc","window":{"width":320,"height":480,"minWidth":260}}"#,
        )
        .unwrap();
        std::fs::write(dir.join("index.html"), "<h1>x</h1>").unwrap();
        let no_win = root.join("apps").join("plain");
        std::fs::create_dir_all(&no_win).unwrap();
        std::fs::write(no_win.join("app.json"), r#"{"name":"Plain"}"#).unwrap();
        std::fs::write(no_win.join("index.html"), "<h1>x</h1>").unwrap();

        let apps = scan_apps(&root);
        let calc = apps.iter().find(|a| a.id == "calc").unwrap();
        let w = calc.window.as_ref().expect("window parsed");
        assert_eq!((w.width, w.height, w.min_width, w.min_height), (Some(320), Some(480), Some(260), None));
        // apps without a window block get None (back-compat)
        assert!(apps.iter().find(|a| a.id == "plain").unwrap().window.is_none());

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn visibility_defaults_private_and_parses_public() {
        let root = std::env::temp_dir().join(format!("liquid-vis-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (id, manifest) in [
            ("open", r#"{"name":"Open","visibility":"public"}"#),
            ("closed", r#"{"name":"Closed","visibility":"private"}"#),
            ("legacy", r#"{"name":"Legacy"}"#),
        ] {
            let dir = root.join("apps").join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("app.json"), manifest).unwrap();
            std::fs::write(dir.join("index.html"), "<h1>x</h1>").unwrap();
        }
        let apps = scan_apps(&root);
        let vis = |id: &str| apps.iter().find(|a| a.id == id).unwrap().visibility;
        assert_eq!(vis("open"), Visibility::Public);
        assert_eq!(vis("closed"), Visibility::Private);
        // no visibility field = private: apps must opt IN to guests
        assert_eq!(vis("legacy"), Visibility::Private);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn backend_declarations_parse_synthesize_and_validate() {
        let root = std::env::temp_dir().join(format!("liquid-be-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let make = |id: &str, manifest: &str, files: &[&str]| {
            let dir = root.join("apps").join(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("app.json"), manifest).unwrap();
            for f in files {
                let p = dir.join(f);
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(p, "x").unwrap();
            }
        };
        // declared polyglot runner (+ env, + health)
        make(
            "phx",
            r#"{"name":"Phx","surface":"full","backend":{"run":["mix","phx.server"],"health":"/health","env":{"MIX_ENV":"prod"}}}"#,
            &["mix.exs"],
        );
        // legacy zero-config bun backend, synthesized
        make("legacy", r#"{"name":"Legacy"}"#, &["index.html", "backend/index.ts"]);
        // declared run wins over a present backend/index.ts
        make(
            "custom",
            r#"{"name":"Custom","backend":{"run":["bun","run","server.ts"]}}"#,
            &["index.html", "server.ts", "backend/index.ts"],
        );
        // invalid declarations are skipped, not half-run
        make("emptyrun", r#"{"name":"E","backend":{"run":[]}}"#, &["index.html"]);
        make(
            "badhealth",
            r#"{"name":"B","backend":{"run":["bun","x"],"health":"health"}}"#,
            &["index.html"],
        );
        // full surface without a backend has nothing to serve
        make("fullnobackend", r#"{"name":"F","surface":"full"}"#, &["index.html"]);
        // panel app without index.html is not an app
        make("noindex", r#"{"name":"N","backend":{"run":["bun","x"]}}"#, &[]);

        let apps = scan_apps(&root);
        let ids: Vec<&str> = apps.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, vec!["custom", "legacy", "phx"]);

        let get = |id: &str| apps.iter().find(|a| a.id == id).unwrap();
        let phx = get("phx");
        assert_eq!(phx.surface, Surface::Full);
        let spec = phx.backend.as_ref().unwrap();
        assert_eq!(spec.run, vec!["mix", "phx.server"]);
        assert_eq!(spec.health.as_deref(), Some("/health"));
        assert_eq!(spec.env.get("MIX_ENV").map(String::as_str), Some("prod"));
        assert!(phx.has_backend);

        let legacy = get("legacy");
        assert_eq!(legacy.surface, Surface::Panel);
        assert_eq!(legacy.backend.as_ref().unwrap().run, vec!["bun", "run", BUN_BACKEND_ENTRY]);

        assert_eq!(get("custom").backend.as_ref().unwrap().run, vec!["bun", "run", "server.ts"]);

        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn manifest_wire_shape_hides_the_runner_spec() {
        // Clients get `surface` and `has_backend`; the `backend` key on the
        // wire is the runtime STATUS injected by enriched_apps — the runner
        // spec must not collide with it.
        let manifest = AppManifest {
            id: "x".into(),
            name: "X".into(),
            icon: "🧪".into(),
            description: String::new(),
            has_backend: true,
            window: None,
            visibility: Visibility::Private,
            surface: Surface::Full,
            backend: Some(BackendSpec {
                run: vec!["mix".into(), "phx.server".into()],
                health: None,
                env: HashMap::new(),
            }),
        };
        let wire = serde_json::to_value(&manifest).unwrap();
        assert_eq!(wire["surface"], "full");
        assert_eq!(wire["has_backend"], true);
        assert!(wire.get("backend").is_none());
    }

    #[test]
    fn app_access_rule_is_exhaustive() {
        use Visibility::{Private, Public};
        // public: open to everyone, authorized or not
        assert!(app_access_allowed(Public, false));
        assert!(app_access_allowed(Public, true));
        // private: only an authorized caller (owner session or screenshot cap)
        assert!(app_access_allowed(Private, true));
        // private: unauthorized is DENIED — loopback is NOT a free pass, so a
        // guest-reachable app backend can't proxy-read private apps
        assert!(!app_access_allowed(Private, false));
    }

    #[test]
    fn shot_query_matches_only_the_exact_secret() {
        assert!(shot_in_query("s3cr3t", Some("__lshot=s3cr3t")));
        assert!(shot_in_query("s3cr3t", Some("a=1&__lshot=s3cr3t&b=2")));
        assert!(!shot_in_query("s3cr3t", Some("__lshot=wrong")));
        assert!(!shot_in_query("s3cr3t", Some("__lshotx=s3cr3t")));
        assert!(!shot_in_query("s3cr3t", None));
        // an empty secret is never a capability (belt-and-suspenders)
        assert!(!shot_in_query("", Some("__lshot=")));
    }
}
