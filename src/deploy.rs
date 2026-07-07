//! The deploy pipeline and its anti-forgery boundary.
//!
//! The agent works and commits in the workspace repo (`workspace_dir`),
//! advancing HEAD. Apps are *served* from a separate git worktree
//! (`served_dir`, under supervisor-owned `$DATA_DIR/pipeline/`) checked out
//! at the deployed commit. The supervisor only ever checks that worktree out
//! to a commit the pipeline approved — so "what the agent wrote" and "what is
//! live" are distinct, and the only path to production is passing review.
//!
//! - vibe mode: every workspace commit deploys immediately (today's behavior).
//! - reviewed mode: commits that touch `apps/` are reviewed by a subagent
//!   first; on rejection the deployed commit is left untouched and the human
//!   is asked to decide. Non-app commits (memory, SHELL.json, CRONS) never
//!   gate — those files are read live from the workspace, not the worktree.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use anyhow::{Context, Result};
use serde::Serialize;
use tracing::{info, warn};

const PIPELINE_SUBDIR: &str = "pipeline";
const DEPLOYED_SUBDIR: &str = "deployed";
const REVIEWS_SUBDIR: &str = "reviews";
const MODE_SETTING: &str = "pipeline_mode";
const APPS_PREFIX: &str = "apps/";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PipelineMode {
    Vibe,
    Reviewed,
}

impl PipelineMode {
    pub fn parse(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "vibe" => Some(Self::Vibe),
            "reviewed" => Some(Self::Reviewed),
            _ => None,
        }
    }
    fn as_str(self) -> &'static str {
        match self {
            Self::Vibe => "vibe",
            Self::Reviewed => "reviewed",
        }
    }
}

/// What the shell shows about the pipeline right now.
#[derive(Clone, Debug, Serialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum PipelineStatus {
    /// Everything the agent committed is live.
    Clean,
    /// A commit is being reviewed.
    Reviewing { candidate: String },
    /// Review rejected a commit; the human must approve or the agent must fix.
    Rejected {
        candidate: String,
        reasoning: String,
    },
}

pub struct DeployManager {
    workspace_dir: PathBuf,
    served_dir: PathBuf,
    reviews_dir: PathBuf,
    mode: Mutex<PipelineMode>,
    status: Mutex<PipelineStatus>,
}

impl DeployManager {
    /// Create (or reattach) the deployed worktree and load the saved mode.
    /// Returns the manager; `served_dir()` is where apps are served from.
    pub fn init(
        workspace_dir: &Path,
        data_dir: &Path,
        default_mode: PipelineMode,
        saved_mode: Option<String>,
    ) -> Result<Self> {
        let pipeline = data_dir.join(PIPELINE_SUBDIR);
        let served_dir = pipeline.join(DEPLOYED_SUBDIR);
        let reviews_dir = pipeline.join(REVIEWS_SUBDIR);
        std::fs::create_dir_all(&reviews_dir).context("creating pipeline dir")?;

        ensure_worktree(workspace_dir, &served_dir)?;

        let mode = saved_mode
            .as_deref()
            .and_then(PipelineMode::parse)
            .unwrap_or(default_mode);
        info!("pipeline mode: {}", mode.as_str());

        Ok(Self {
            workspace_dir: workspace_dir.to_path_buf(),
            served_dir,
            reviews_dir,
            mode: Mutex::new(mode),
            status: Mutex::new(PipelineStatus::Clean),
        })
    }

    pub fn served_dir(&self) -> &Path {
        &self.served_dir
    }

    pub fn mode(&self) -> PipelineMode {
        *self.mode.lock().expect("mode poisoned")
    }

    pub fn set_mode(&self, mode: PipelineMode) {
        *self.mode.lock().expect("mode poisoned") = mode;
    }

    pub fn status(&self) -> PipelineStatus {
        self.status.lock().expect("status poisoned").clone()
    }

    fn set_status(&self, status: PipelineStatus) {
        *self.status.lock().expect("status poisoned") = status;
    }

    pub fn deployed_commit(&self) -> Result<String> {
        rev_parse(&self.served_dir, "HEAD")
    }

    pub fn head_commit(&self) -> Result<String> {
        rev_parse(&self.workspace_dir, "HEAD")
    }

    /// Files changed between the deployed commit and workspace HEAD.
    pub fn undeployed_changes(&self) -> Result<Vec<String>> {
        let deployed = self.deployed_commit()?;
        let head = self.head_commit()?;
        if deployed == head {
            return Ok(Vec::new());
        }
        diff_names(&self.workspace_dir, &deployed, &head)
    }

    fn apps_changed(&self, files: &[String]) -> bool {
        files.iter().any(|f| f.starts_with(APPS_PREFIX))
    }

    /// Check the deployed worktree out to `commit`. Ignored files
    /// (node_modules, data/) are preserved — git checkout only touches
    /// tracked files.
    pub fn deploy(&self, commit: &str) -> Result<()> {
        git(&self.served_dir, &["checkout", "--quiet", "--detach", commit])
            .with_context(|| format!("deploying {commit}"))?;
        self.set_status(PipelineStatus::Clean);
        info!("deployed {commit}");
        Ok(())
    }

    /// The diff a reviewer sees: computed by the supervisor from git, never
    /// supplied by the agent.
    pub fn review_diff(&self) -> Result<String> {
        let deployed = self.deployed_commit()?;
        let head = self.head_commit()?;
        let out = Command::new("git")
            .args(["diff", &format!("{deployed}..{head}")])
            .current_dir(&self.workspace_dir)
            .output()
            .context("computing review diff")?;
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    /// Record a review verdict in the supervisor-owned reviews dir.
    pub fn record_review(&self, commit: &str, verdict: &str, reasoning: &str) {
        let path = self.reviews_dir.join(format!("{commit}-reviewer.md"));
        let body = format!("# Review of {commit}\n\nVerdict: {verdict}\n\n{reasoning}\n");
        if let Err(err) = std::fs::write(&path, body) {
            warn!("could not write review record: {err}");
        }
    }

    pub fn mark_reviewing(&self, candidate: &str) {
        self.set_status(PipelineStatus::Reviewing {
            candidate: candidate.to_string(),
        });
    }

    pub fn mark_rejected(&self, candidate: &str, reasoning: &str) {
        self.set_status(PipelineStatus::Rejected {
            candidate: candidate.to_string(),
            reasoning: reasoning.to_string(),
        });
    }

    /// Decide what to do after the agent commits. Returns the commit that
    /// should be reviewed (reviewed mode, apps changed), or None if the
    /// change was deployed immediately / nothing to do.
    pub fn reconcile(&self) -> Result<Option<String>> {
        let files = self.undeployed_changes()?;
        if files.is_empty() {
            return Ok(None);
        }
        let head = self.head_commit()?;
        // Non-app changes are read live from the workspace, not the worktree —
        // advance the deployed pointer without review so the worktree tracks
        // HEAD, but they were never gated.
        if self.mode() == PipelineMode::Vibe || !self.apps_changed(&files) {
            self.deploy(&head)?;
            return Ok(None);
        }
        // reviewed mode + apps changed: gate it.
        self.mark_reviewing(&head);
        Ok(Some(head))
    }

    pub fn persist_mode(&self, db: &crate::db::Db) {
        let _ = db.set_setting(MODE_SETTING, self.mode().as_str());
    }
}

// --- git plumbing ----------------------------------------------------------------

fn ensure_worktree(workspace_dir: &Path, served_dir: &Path) -> Result<()> {
    if served_dir.join(".git").exists() {
        return Ok(()); // already a worktree (restart)
    }
    if served_dir.exists() {
        // Stale non-worktree directory — clear it so `worktree add` succeeds.
        std::fs::remove_dir_all(served_dir).context("clearing stale served dir")?;
    }
    // Prune any dangling registration from a previous run, then add detached
    // at the current workspace HEAD.
    let _ = git(workspace_dir, &["worktree", "prune"]);
    let head = rev_parse(workspace_dir, "HEAD")?;
    git(
        workspace_dir,
        &[
            "worktree",
            "add",
            "--quiet",
            "--detach",
            &served_dir.to_string_lossy(),
            &head,
        ],
    )
    .context("creating deployed worktree")?;
    info!("created deployed worktree at {}", served_dir.display());
    Ok(())
}

fn rev_parse(dir: &Path, rev: &str) -> Result<String> {
    let out = Command::new("git")
        .args(["rev-parse", rev])
        .current_dir(dir)
        .output()
        .context("git rev-parse")?;
    anyhow::ensure!(out.status.success(), "git rev-parse {rev} failed");
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn diff_names(dir: &Path, from: &str, to: &str) -> Result<Vec<String>> {
    let out = Command::new("git")
        .args(["diff", "--name-only", &format!("{from}..{to}")])
        .current_dir(dir)
        .output()
        .context("git diff --name-only")?;
    anyhow::ensure!(out.status.success(), "git diff failed");
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .collect())
}

fn git(dir: &Path, args: &[&str]) -> Result<()> {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .context("running git")?;
    anyhow::ensure!(status.success(), "git {args:?} exited with {status}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(dir: &Path, args: &[&str]) {
        let ok = Command::new("git").args(args).current_dir(dir).status().unwrap().success();
        assert!(ok, "git {args:?} failed");
    }

    fn init_repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        run(dir, &["init", "--quiet", "--initial-branch=main"]);
        run(dir, &["config", "user.name", "test"]);
        run(dir, &["config", "user.email", "t@t"]);
        std::fs::write(dir.join("README.md"), "x").unwrap();
        run(dir, &["add", "-A"]);
        run(dir, &["commit", "--quiet", "-m", "init"]);
    }

    fn commit(dir: &Path, path: &str, content: &str, msg: &str) -> String {
        let full = dir.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, content).unwrap();
        run(dir, &["add", "-A"]);
        run(dir, &["commit", "--quiet", "-m", msg]);
        rev_parse(dir, "HEAD").unwrap()
    }

    fn setup() -> (PathBuf, PathBuf, DeployManager) {
        let base = std::env::temp_dir().join(format!(
            "liquid-deploy-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        let workspace = base.join("workspace");
        let data = base.join("data");
        init_repo(&workspace);
        let mgr = DeployManager::init(&workspace, &data, PipelineMode::Vibe, None).unwrap();
        (workspace, data, mgr)
    }

    #[test]
    fn vibe_deploys_every_commit() {
        let (workspace, _data, mgr) = setup();
        assert_eq!(mgr.status(), PipelineStatus::Clean);
        assert!(mgr.undeployed_changes().unwrap().is_empty());

        commit(&workspace, "apps/calc/index.html", "<h1>calc</h1>", "add calc");
        // agent committed; reconcile should auto-deploy in vibe mode
        assert_eq!(mgr.reconcile().unwrap(), None);
        assert_eq!(mgr.deployed_commit().unwrap(), mgr.head_commit().unwrap());
        // the served worktree now has the app file
        assert!(mgr.served_dir().join("apps/calc/index.html").exists());

        std::fs::remove_dir_all(workspace.parent().unwrap()).ok();
    }

    #[test]
    fn reviewed_mode_gates_app_changes_but_not_memory() {
        let (workspace, _data, mgr) = setup();
        mgr.set_mode(PipelineMode::Reviewed);

        // A memory-file change deploys without review even in reviewed mode.
        commit(&workspace, "MEMORY.md", "learned things", "update memory");
        assert_eq!(mgr.reconcile().unwrap(), None);
        assert_eq!(mgr.deployed_commit().unwrap(), mgr.head_commit().unwrap());

        // An app change is gated: reconcile returns the candidate, worktree
        // stays put, status is Reviewing.
        let deployed_before = mgr.deployed_commit().unwrap();
        let candidate = commit(&workspace, "apps/x/index.html", "<h1>x</h1>", "add x");
        assert_eq!(mgr.reconcile().unwrap(), Some(candidate.clone()));
        assert_eq!(mgr.deployed_commit().unwrap(), deployed_before);
        assert!(!mgr.served_dir().join("apps/x/index.html").exists());
        assert_eq!(mgr.status(), PipelineStatus::Reviewing { candidate: candidate.clone() });

        // Approving deploys it.
        mgr.deploy(&candidate).unwrap();
        assert_eq!(mgr.deployed_commit().unwrap(), candidate);
        assert!(mgr.served_dir().join("apps/x/index.html").exists());
        assert_eq!(mgr.status(), PipelineStatus::Clean);

        std::fs::remove_dir_all(workspace.parent().unwrap()).ok();
    }

    #[test]
    fn worktree_reattaches_on_restart() {
        let (workspace, data, mgr) = setup();
        commit(&workspace, "apps/a/index.html", "a", "add a");
        mgr.reconcile().unwrap();
        let deployed = mgr.deployed_commit().unwrap();
        drop(mgr);

        // Re-init with the same dirs (simulating a supervisor restart).
        let mgr2 = DeployManager::init(&workspace, &data, PipelineMode::Vibe, None).unwrap();
        assert_eq!(mgr2.deployed_commit().unwrap(), deployed);

        std::fs::remove_dir_all(workspace.parent().unwrap()).ok();
    }
}
