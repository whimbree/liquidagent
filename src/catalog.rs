//! The built-in app library: apps under `default-workspace/apps/` ship
//! embedded in the binary (staged by build.rs, junk filtered) and can be
//! installed into the live workspace at ANY time, not just first boot.
//!
//! Installing copies the app in and commits it — from then on it's the
//! owner's, evolved by the agent like anything else it grew. The library
//! copy never auto-updates an installed app; instead, updates are explicit
//! and git-native:
//!
//! - a `.library.json` marker (committed with the install) records the
//!   fingerprint of the library content that was installed, so "upstream
//!   changed" is an exact question;
//! - the install/update commit is the merge BASE, the workspace copy is
//!   OURS, the new library copy is THEIRS — "update" is a real 3-way git
//!   merge, so local evolution survives an upstream refresh;
//! - conflicts stop before any commit and sit in the working tree, where
//!   the agent (or, later, the in-shell IDE) resolves them like any merge;
//! - "replace" overwrites with the pristine library copy — git history
//!   still keeps what you had.

use std::path::{Path, PathBuf};

use anyhow::Context;
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use include_dir::{include_dir, Dir};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use crate::apps::{self, BackendSpec};
use crate::AppState;

/// Staged by build.rs from default-workspace/apps (minus _build/data/…).
static CATALOG: Dir<'static> = include_dir!("$OUT_DIR/catalog");

/// Committed alongside an installed app: which library content it came from.
pub const MARKER_FILE: &str = ".library.json";

/// One library entry, as listed to the shell.
#[derive(serde::Serialize)]
pub struct CatalogEntry {
    pub id: String,
    pub name: String,
    pub icon: String,
    pub description: String,
    pub visibility: apps::Visibility,
    pub surface: apps::Surface,
    /// argv[0] of the app's backend, if it has one ("bun", "mix", …).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runtime: Option<String>,
    /// Whether that runtime is on the supervisor's PATH right now.
    pub runtime_available: bool,
    pub installed: bool,
    /// Installed AND present in the deployed worktree — i.e. actually being
    /// served. False with installed=true means the install/update committed
    /// (or extracted) but never deployed: exactly the state that otherwise
    /// reads as a lying "Installed ✓" next to a 404.
    pub live: bool,
    /// The library ships newer content than what was installed (or the
    /// baseline is unknown — pre-marker installs — and we can't rule it out).
    pub update_available: bool,
    /// The workspace copy has evolved since it was installed/updated.
    pub local_changes: bool,
}

// --- embedded-content helpers ------------------------------------------------

/// The backend an embedded app would run with: its declared block, or the
/// synthesized Bun default when the embedded tree has backend/index.ts.
/// (Same rule as apps::resolve_backend, against the embedded dir.)
fn embedded_backend(dir: &Dir, declared: Option<BackendSpec>) -> Option<BackendSpec> {
    declared.or_else(|| {
        dir.get_file(dir.path().join(apps::BUN_BACKEND_ENTRY))
            .map(|_| apps::default_bun_spec())
    })
}

fn walk_files<'a>(dir: &'a Dir<'a>, all: &mut Vec<&'a include_dir::File<'a>>) {
    all.extend(dir.files());
    for sub in dir.dirs() {
        walk_files(sub, all);
    }
}

/// Content fingerprint of an embedded app: sha256 over sorted (path, bytes).
fn embedded_hash(dir: &Dir) -> String {
    let mut files = Vec::new();
    walk_files(dir, &mut files);
    files.sort_by_key(|f| f.path().to_path_buf());
    let mut hasher = Sha256::new();
    for file in files {
        hasher.update(file.path().to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher.update(file.contents());
        hasher.update([0]);
    }
    data_encoding::HEXLOWER.encode(&hasher.finalize())
}

fn on_path(cmd: &str) -> bool {
    if cmd.contains('/') {
        return Path::new(cmd).is_file();
    }
    std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).any(|dir| dir.join(cmd).is_file()))
        .unwrap_or(false)
}

// --- workspace-state helpers ---------------------------------------------------

#[derive(Deserialize)]
struct Marker {
    library_hash: String,
}

fn installed_marker(workspace_dir: &Path, id: &str) -> Option<Marker> {
    let raw = std::fs::read_to_string(workspace_dir.join("apps").join(id).join(MARKER_FILE)).ok()?;
    serde_json::from_str(&raw).ok()
}

/// The merge base for a library app: the last install/update commit (the one
/// that wrote the marker), or — for installs that predate markers — the
/// commit that created the app (its content then was pristine library copy).
fn base_commit(workspace_dir: &Path, id: &str) -> Option<String> {
    let marker_path = format!("apps/{id}/{MARKER_FILE}");
    let last_marker = git_stdout(workspace_dir, &["log", "-1", "--format=%H", "--", &marker_path])
        .ok()
        .filter(|s| !s.is_empty());
    last_marker.or_else(|| {
        git_stdout(
            workspace_dir,
            &["rev-list", "--reverse", "HEAD", "--", &format!("apps/{id}/{}", apps::MANIFEST_FILE)],
        )
        .ok()
        .and_then(|out| out.lines().next().map(str::to_string))
    })
}

/// Has the workspace copy evolved since its baseline? (Committed changes
/// since the base, or uncommitted ones right now — marker changes excluded.)
fn has_local_changes(workspace_dir: &Path, id: &str, base: &str) -> bool {
    let path = format!("apps/{id}");
    let committed = git_stdout(workspace_dir, &["diff", "--name-only", base, "HEAD", "--", &path])
        .map(|out| out.lines().any(|l| !l.ends_with(MARKER_FILE)))
        .unwrap_or(true);
    let dirty = git_stdout(workspace_dir, &["status", "--porcelain", "--", &path])
        .map(|out| !out.trim().is_empty())
        .unwrap_or(true);
    committed || dirty
}

fn entries(workspace_dir: &Path, served_dir: &Path) -> Vec<CatalogEntry> {
    let mut list: Vec<CatalogEntry> = CATALOG
        .dirs()
        .filter_map(|dir| {
            let id = dir.path().file_name()?.to_str()?.to_string();
            let manifest_file = dir.get_file(dir.path().join(apps::MANIFEST_FILE))?;
            let raw = apps::parse_raw_manifest(manifest_file.contents_utf8()?)
                .map_err(|err| warn!("catalog app {id}: bad manifest: {err}"))
                .ok()?;
            let backend = embedded_backend(dir, raw.backend);
            let runtime = backend.and_then(|spec| spec.run.into_iter().next());
            let installed = workspace_dir.join("apps").join(&id).exists();
            let live = installed && served_dir.join("apps").join(&id).join(apps::MANIFEST_FILE).exists();
            let (update_available, local_changes) = if installed {
                // Baseline unknown (pre-marker install) counts as updatable —
                // we can't rule an upstream change out, and merge still works
                // off the creation commit.
                let stale = installed_marker(workspace_dir, &id)
                    .map(|m| m.library_hash != embedded_hash(dir))
                    .unwrap_or(true);
                let local = base_commit(workspace_dir, &id)
                    .map(|base| has_local_changes(workspace_dir, &id, &base))
                    .unwrap_or(true);
                (stale, local)
            } else {
                (false, false)
            };
            Some(CatalogEntry {
                installed,
                live,
                update_available,
                local_changes,
                name: raw.name,
                icon: raw.icon.unwrap_or_else(|| "📦".to_string()),
                description: raw.description.unwrap_or_default(),
                visibility: raw.visibility,
                surface: raw.surface,
                runtime_available: runtime.as_deref().map(on_path).unwrap_or(true),
                runtime,
                id,
            })
        })
        .collect();
    list.sort_by(|a, b| a.id.cmp(&b.id));
    list
}

// --- extraction ----------------------------------------------------------------

/// Extract an embedded app into `<workspace>/apps/<id>` and write its
/// baseline marker. Every path comes from the embedded (build-staged) tree,
/// so there is no traversal surface.
pub fn extract_app(id: &str, workspace_dir: &Path) -> anyhow::Result<()> {
    let dir = CATALOG
        .get_dir(id)
        .with_context(|| format!("no {id} in the built-in library"))?;
    write_app_content(dir, &workspace_dir.join("apps"))?;
    anyhow::ensure!(
        workspace_dir.join("apps").join(id).join(apps::MANIFEST_FILE).exists(),
        "{id} extracted without a manifest"
    );
    Ok(())
}

/// Write an embedded app's files + marker under `apps_root` (which contains
/// the `<id>/…` tree).
fn write_app_content(dir: &Dir, apps_root: &Path) -> anyhow::Result<()> {
    let mut files = Vec::new();
    walk_files(dir, &mut files);
    for file in files {
        // Embedded paths are relative to the catalog root ("<id>/…").
        let dest = apps_root.join(file.path());
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&dest, file.contents()).with_context(|| format!("writing {}", dest.display()))?;
    }
    let marker = json!({ "library_hash": embedded_hash(dir) });
    std::fs::write(
        apps_root.join(dir.path()).join(MARKER_FILE),
        serde_json::to_string_pretty(&marker).context("marker json")?,
    )
    .context("writing library marker")?;
    Ok(())
}

// --- the 3-way library merge -----------------------------------------------------

pub enum MergeOutcome {
    /// Merged (or already current); HEAD is ready to deploy.
    Merged,
    /// The merge stopped on conflicts; they sit in the working tree for the
    /// agent (or a human) to resolve — nothing was committed or deployed.
    Conflicts { files: Vec<String> },
}

/// Merge new library content for `id` onto the workspace copy.
/// `write_content` writes the NEW library tree under the given apps-root —
/// injectable so tests can simulate upstream change (the embedded catalog is
/// fixed at compile time).
fn merge_update(
    workspace_dir: &Path,
    id: &str,
    base: &str,
    write_content: &dyn Fn(&Path) -> anyhow::Result<()>,
) -> anyhow::Result<MergeOutcome> {
    // Build THEIRS: a commit on top of the base whose apps/<id> is the new
    // library content — staged in a temporary worktree so the live checkout
    // is untouched until the actual merge.
    static MERGE_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seq = MERGE_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let tmp = std::env::temp_dir().join(format!("liquid-libmerge-{id}-{}-{seq}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    let tmp_str = tmp.to_string_lossy().to_string();
    git(workspace_dir, &["worktree", "add", "--quiet", "--detach", &tmp_str, base])?;
    let theirs = (|| -> anyhow::Result<String> {
        let app_path = format!("apps/{id}");
        std::fs::remove_dir_all(tmp.join(&app_path)).ok();
        write_content(&tmp.join("apps"))?;
        git(&tmp, &["add", "-A", "--", &app_path])?;
        // --allow-empty: identical content still yields a commit, making the
        // subsequent merge a clean no-op instead of an error.
        git(
            &tmp,
            &["commit", "--quiet", "--allow-empty", "-m", &format!("Library version of {id}")],
        )?;
        git_stdout(&tmp, &["rev-parse", "HEAD"])
    })();
    let _ = git(workspace_dir, &["worktree", "remove", "--force", &tmp_str]);
    let _ = std::fs::remove_dir_all(&tmp);
    let theirs = theirs?;

    // OURS is the live workspace. Merge; conflicts deliberately stay in the
    // working tree (no commit, nothing deploys) — that's the resolve surface.
    let merged = git(
        workspace_dir,
        &[
            "merge",
            "--no-edit",
            "-m",
            &format!("Merge library update of {id}"),
            &theirs,
        ],
    );
    match merged {
        Ok(()) => Ok(MergeOutcome::Merged),
        Err(_) => {
            let files = git_stdout(workspace_dir, &["diff", "--name-only", "--diff-filter=U"])
                .unwrap_or_default()
                .lines()
                .map(str::to_string)
                .collect::<Vec<_>>();
            if files.is_empty() {
                // Not a conflict — a real failure. Leave nothing half-done.
                let _ = git(workspace_dir, &["merge", "--abort"]);
                anyhow::bail!("library merge of {id} failed for a non-conflict reason");
            }
            Ok(MergeOutcome::Conflicts { files })
        }
    }
}

// --- endpoints -------------------------------------------------------------------

/// GET /api/catalog — the built-in library with install/update state.
pub async fn list(State(state): State<AppState>) -> Json<serde_json::Value> {
    Json(json!({ "apps": entries(&state.workspace_dir, &state.served_dir) }))
}

/// Shared preconditions for install/update: the target must be a library app
/// and the pipeline must be quiet (a direct post-commit deploy must never
/// sneak unreviewed agent commits out).
fn guard(state: &AppState, app: &str) -> Result<(), Response> {
    if !CATALOG.get_dir(app).is_some() {
        return Err((StatusCode::NOT_FOUND, Json(json!({ "error": "no such app in the library" }))).into_response());
    }
    match state.deploy.undeployed_changes() {
        Ok(pending) if pending.is_empty() => Ok(()),
        Ok(_) => Err((
            StatusCode::CONFLICT,
            Json(json!({ "error": "the workspace has undeployed changes (pending review?) — resolve those first" })),
        )
            .into_response()),
        Err(err) => {
            warn!("catalog: pipeline check failed: {err:#}");
            Err(StatusCode::INTERNAL_SERVER_ERROR.into_response())
        }
    }
}

fn commit_and_deploy(state: &AppState, app: &str, message: &str) -> Response {
    let pathspec = format!("apps/{app}");
    let committed = git(&state.workspace_dir, &["add", "--", &pathspec]).and_then(|()| {
        // Pathspec-scoped commit: a dirty workspace (agent mid-thought) must
        // not get swept into a library commit.
        git(&state.workspace_dir, &["commit", "--quiet", "-m", message, "--", &pathspec])
    });
    if let Err(err) = committed {
        warn!("catalog {app}: commit failed: {err:#}");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": format!("files extracted but the commit failed: {err:#}") })),
        )
            .into_response();
    }
    deploy_head(state, app)
}

/// The bytes are platform-shipped and the human asked, so library commits are
/// pre-approved: deploy directly rather than riding the agent review gate.
/// VERIFIES the app actually landed in the served worktree — a success answer
/// must mean "it is being served", never "the commit probably worked".
fn deploy_head(state: &AppState, app: &str) -> Response {
    let deployed = state
        .deploy
        .head_commit()
        .and_then(|head| state.deploy.deploy(&head).map(|()| head));
    match deployed {
        Ok(head) => {
            crate::refresh_served_apps_pub(state);
            if !state.served_dir.join("apps").join(app).join(apps::MANIFEST_FILE).exists() {
                warn!("catalog {app}: deployed {head} but the app is missing from the served worktree");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(json!({
                        "error": "committed, but the app did not land in the served worktree — check the supervisor log (was everything gitignored?)",
                        "commit": head,
                    })),
                )
                    .into_response();
            }
            info!("library: deployed {app} at {head}");
            (StatusCode::OK, Json(json!({ "app": app, "commit": head }))).into_response()
        }
        Err(err) => {
            warn!("catalog {app}: deploy failed: {err:#}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("committed but not deployed: {err:#}") })),
            )
                .into_response()
        }
    }
}

/// POST /api/catalog/{app}/install — copy a library app into the workspace,
/// commit, deploy.
pub async fn install(State(state): State<AppState>, UrlPath(app): UrlPath<String>) -> Response {
    if let Err(denied) = guard(&state, &app) {
        return denied;
    }
    if state.workspace_dir.join("apps").join(&app).exists() {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "already in your workspace — use update, or ask liquid to change it" })),
        )
            .into_response();
    }
    if let Err(err) = extract_app(&app, &state.workspace_dir) {
        warn!("catalog install of {app} failed: {err:#}");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }
    commit_and_deploy(&state, &app, &format!("Install {app} from the built-in app library"))
}

#[derive(Deserialize)]
pub struct UpdateBody {
    /// "merge" pulls the new library version ON TOP of local changes
    /// (3-way; conflicts stop and wait in the working tree).
    /// "replace" discards local evolution for the pristine library copy
    /// (git history still keeps it).
    mode: UpdateMode,
}

#[derive(Deserialize, Clone, Copy, PartialEq)]
#[serde(rename_all = "lowercase")]
enum UpdateMode {
    Merge,
    Replace,
}

/// POST /api/catalog/{app}/update {"mode": "merge" | "replace"}
pub async fn update(
    State(state): State<AppState>,
    UrlPath(app): UrlPath<String>,
    Json(body): Json<UpdateBody>,
) -> Response {
    if let Err(denied) = guard(&state, &app) {
        return denied;
    }
    let app_dir = state.workspace_dir.join("apps").join(&app);
    if !app_dir.exists() {
        return (StatusCode::CONFLICT, Json(json!({ "error": "not installed — install it instead" }))).into_response();
    }
    // Refuse mid-merge or with uncommitted app changes: merging onto a dirty
    // tree loses work, and a second merge on top of conflicts compounds them.
    if state.workspace_dir.join(".git").join("MERGE_HEAD").exists() {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "a merge is already in progress — ask liquid to finish resolving it first" })),
        )
            .into_response();
    }
    let dirty = git_stdout(&state.workspace_dir, &["status", "--porcelain", "--", &format!("apps/{app}")])
        .map(|out| !out.trim().is_empty())
        .unwrap_or(true);
    if dirty {
        return (
            StatusCode::CONFLICT,
            Json(json!({ "error": "the app has uncommitted changes — ask liquid to commit or discard them first" })),
        )
            .into_response();
    }

    match body.mode {
        UpdateMode::Replace => {
            if let Err(err) = std::fs::remove_dir_all(&app_dir) {
                warn!("catalog replace of {app}: clearing failed: {err:#}");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            if let Err(err) = extract_app(&app, &state.workspace_dir) {
                warn!("catalog replace of {app} failed: {err:#}");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
            commit_and_deploy(&state, &app, &format!("Replace {app} with the current library version"))
        }
        UpdateMode::Merge => {
            let Some(base) = base_commit(&state.workspace_dir, &app) else {
                return (
                    StatusCode::CONFLICT,
                    Json(json!({ "error": "no baseline commit found for this app — use replace instead" })),
                )
                    .into_response();
            };
            let write = |apps_root: &Path| -> anyhow::Result<()> {
                let dir = CATALOG.get_dir(&app).context("library app vanished")?;
                write_app_content(dir, apps_root)
            };
            match merge_update(&state.workspace_dir, &app, &base, &write) {
                Ok(MergeOutcome::Merged) => deploy_head(&state, &app),
                Ok(MergeOutcome::Conflicts { files }) => (
                    StatusCode::CONFLICT,
                    Json(json!({
                        "conflicts": files,
                        "error": "the update conflicts with your local changes — they're waiting in the working tree; ask liquid to resolve the merge",
                    })),
                )
                    .into_response(),
                Err(err) => {
                    warn!("catalog merge of {app} failed: {err:#}");
                    StatusCode::INTERNAL_SERVER_ERROR.into_response()
                }
            }
        }
    }
}

// --- git plumbing ----------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) -> anyhow::Result<()> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .context("running git")?;
    // Carry stderr into the error: these surface in the shell UI, where
    // "git exited with 1" is useless and "nothing to commit" is the answer.
    anyhow::ensure!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}

fn git_stdout(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .context("running git")?;
    anyhow::ensure!(out.status.success(), "git {args:?} failed");
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_library_ships_stronglifts_and_whiteboard() {
        let tmp = std::env::temp_dir().join(format!("liquid-catalog-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("apps")).unwrap();
        let served = tmp.join("served");

        let list = entries(&tmp, &served);
        let ids: Vec<&str> = list.iter().map(|e| e.id.as_str()).collect();
        assert!(ids.contains(&"stronglifts"), "stronglifts in library, got {ids:?}");
        assert!(ids.contains(&"whiteboard"), "whiteboard in library, got {ids:?}");

        let wb = list.iter().find(|e| e.id == "whiteboard").unwrap();
        assert_eq!(wb.runtime.as_deref(), Some("mix"));
        assert_eq!(wb.visibility, apps::Visibility::Public);
        assert_eq!(wb.surface, apps::Surface::Full);
        assert!(!wb.installed && !wb.update_available);
        // stronglifts has a legacy bun backend — synthesized, not declared
        let sl = list.iter().find(|e| e.id == "stronglifts").unwrap();
        assert_eq!(sl.runtime.as_deref(), Some("bun"));

        // the embedded tree must NEVER carry build junk…
        assert!(CATALOG.get_dir("whiteboard/_build").is_none());
        assert!(CATALOG.get_dir("whiteboard/data").is_none());
        // …but vendored deps ARE embedded (offline install contract)
        assert!(CATALOG.get_dir("whiteboard/deps/phoenix").is_some());

        // extraction lands a complete, scannable app with its marker
        extract_app("whiteboard", &tmp).unwrap();
        assert!(tmp.join("apps/whiteboard/mix.exs").exists());
        assert!(tmp.join("apps/whiteboard/priv/static/index.html").exists());
        assert!(tmp.join("apps/whiteboard").join(MARKER_FILE).exists());
        assert!(apps::scan_apps(&tmp).iter().any(|a| a.id == "whiteboard"));
        // extracted into the workspace but NOT deployed → installed, not live
        let after = entries(&tmp, &served);
        let wb_after = after.iter().find(|e| e.id == "whiteboard").unwrap();
        assert!(wb_after.installed && !wb_after.live);

        std::fs::remove_dir_all(&tmp).unwrap();
    }

    // --- merge machinery, against a real git repo with synthetic content ---

    fn run(dir: &Path, args: &[&str]) {
        assert!(
            std::process::Command::new("git").args(args).current_dir(dir).status().unwrap().success(),
            "git {args:?} failed"
        );
    }

    fn setup_repo() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "liquid-libmerge-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(dir.join("apps")).unwrap();
        run(&dir, &["init", "--quiet", "--initial-branch=main"]);
        run(&dir, &["config", "user.name", "test"]);
        run(&dir, &["config", "user.email", "t@t"]);
        std::fs::write(dir.join("README.md"), "ws").unwrap();
        run(&dir, &["add", "-A"]);
        run(&dir, &["commit", "--quiet", "-m", "init"]);
        dir
    }

    /// A fake library version: app `demo` with the given file contents.
    fn version<'a>(files: &'a [(&'a str, &'a str)]) -> impl Fn(&Path) -> anyhow::Result<()> + 'a {
        move |apps_root: &Path| {
            for (name, contents) in files {
                let path = apps_root.join("demo").join(name);
                std::fs::create_dir_all(path.parent().unwrap())?;
                std::fs::write(path, contents)?;
            }
            std::fs::write(apps_root.join("demo").join(MARKER_FILE), r#"{"library_hash":"v"}"#)?;
            Ok(())
        }
    }

    #[test]
    fn library_merge_keeps_local_changes_and_takes_upstream_ones() {
        let ws = setup_repo();
        // install v1
        version(&[("app.json", r#"{"name":"Demo"}"#), ("index.html", "v1"), ("style.css", "plain")])(
            &ws.join("apps"),
        )
        .unwrap();
        run(&ws, &["add", "-A"]);
        run(&ws, &["commit", "--quiet", "-m", "install demo"]);
        let base = base_commit(&ws, "demo").expect("base");

        // local evolution: the human's agent restyles it
        std::fs::write(ws.join("apps/demo/style.css"), "fancy").unwrap();
        run(&ws, &["add", "-A"]);
        run(&ws, &["commit", "--quiet", "-m", "restyle demo"]);
        assert!(has_local_changes(&ws, "demo", &base));

        // upstream v2 changes a DIFFERENT file → clean merge, both survive
        let v2 = version(&[("app.json", r#"{"name":"Demo"}"#), ("index.html", "v2"), ("style.css", "plain")]);
        match merge_update(&ws, "demo", &base, &v2).unwrap() {
            MergeOutcome::Merged => {}
            MergeOutcome::Conflicts { files } => panic!("unexpected conflicts: {files:?}"),
        }
        assert_eq!(std::fs::read_to_string(ws.join("apps/demo/index.html")).unwrap(), "v2");
        assert_eq!(std::fs::read_to_string(ws.join("apps/demo/style.css")).unwrap(), "fancy");

        std::fs::remove_dir_all(&ws).ok();
    }

    #[test]
    fn library_merge_conflicts_stop_uncommitted_in_the_working_tree() {
        let ws = setup_repo();
        version(&[("index.html", "v1"), ("app.json", r#"{"name":"Demo"}"#)])(&ws.join("apps")).unwrap();
        run(&ws, &["add", "-A"]);
        run(&ws, &["commit", "--quiet", "-m", "install demo"]);
        let base = base_commit(&ws, "demo").expect("base");
        let head_before = git_stdout(&ws, &["rev-parse", "HEAD"]).unwrap();

        // both sides edit the same file
        std::fs::write(ws.join("apps/demo/index.html"), "local edit").unwrap();
        run(&ws, &["add", "-A"]);
        run(&ws, &["commit", "--quiet", "-m", "local edit"]);
        let v2 = version(&[("index.html", "upstream edit"), ("app.json", r#"{"name":"Demo"}"#)]);
        match merge_update(&ws, "demo", &base, &v2).unwrap() {
            MergeOutcome::Conflicts { files } => {
                assert!(files.iter().any(|f| f.ends_with("index.html")), "{files:?}");
            }
            MergeOutcome::Merged => panic!("expected conflicts"),
        }
        // nothing committed: HEAD moved only by the local edit, and the
        // conflict markers wait in the tree
        assert_ne!(git_stdout(&ws, &["rev-parse", "HEAD"]).unwrap(), head_before);
        assert!(ws.join(".git/MERGE_HEAD").exists());
        let conflicted = std::fs::read_to_string(ws.join("apps/demo/index.html")).unwrap();
        assert!(conflicted.contains("<<<<<<<"), "conflict markers present");

        std::fs::remove_dir_all(&ws).ok();
    }
}
