//! Opening a tree branch for work: the worktree, git branch and target dir of a code branch, its
//! binding to a work node, and the recovery that starts a node's agent again from its stored launch
//! after a boot or an answered decision.

//!
//! **Where the pieces meet.** `open_branch` and `bind_branch` are the two moves of a branch before
//! its node works (spec R8). Both hold the job's lock (`http::job_lock`) from the read of the
//! manifest to its write, as every writer of a manifest does. `save_launch` keeps what each
//! `launch` was asked to start, so that `relaunch_resumed` (boot) and `answer_and_relaunch` (an
//! answered decision) have a prompt to start the agent from again.
//!
//! **Routes.** `routes()` is merged by `crate::http` beside `tree_job::http::routes()`, not inside
//! it: an `axum` router panics when two merged routers hold the same path.

use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::routing::post;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::SqlitePool;

use super::engine::apply_transition;
use super::http::{self, admission_book, branch_admission, live_sizes_gb, refuse, tree_limits};
use super::model::{Branch, JOB_TERMINAL_STATE};
use super::store::{self, StoreError, TreeJob};
use crate::state::AppState;

/// Where a project's tree worktrees live, relative to its root.
pub const WORKTREES_DIR: &str = ".claude/worktrees";
const DOCS_LAYER: &str = "docs";
const BYTES_PER_GB: f64 = 1_073_741_824.0;

// ----- names and paths -----

/// `<root>/.claude/worktrees/<tree>-<branch>`.
pub fn worktree_path(root: &Path, tree_id: &str, branch: &str) -> PathBuf {
    root.join(WORKTREES_DIR).join(format!("{tree_id}-{branch}"))
}

/// `<dev home>/cargo-target/<tree>-<branch>`: one cargo target dir per branch, off the project.
pub fn target_dir(dev_home: &Path, tree_id: &str, branch: &str) -> PathBuf {
    dev_home
        .join("cargo-target")
        .join(format!("{tree_id}-{branch}"))
}

/// The git branch a tree branch works on.
pub fn git_branch_name(tree_id: &str, branch: &str) -> String {
    format!("tree/{tree_id}/{branch}")
}

/// Where this machine's dev-only state lives: `NUCLEOS_DEV_HOME`, else `<system drive>\nucleos-dev`
/// on Windows, else `$XDG_DATA_HOME/nucleos-dev` or `~/.local/share/nucleos-dev`. It only names the
/// directory: nothing here creates it, the cargo target dirs under it are made by cargo.
pub fn dev_home() -> PathBuf {
    if let Some(home) = std::env::var_os("NUCLEOS_DEV_HOME").filter(|home| !home.is_empty()) {
        return PathBuf::from(home);
    }
    if cfg!(windows) {
        let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_owned());
        return PathBuf::from(format!("{drive}\\")).join("nucleos-dev");
    }
    let base = std::env::var_os("XDG_DATA_HOME")
        .filter(|base| !base.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local").join("share"))
        })
        .unwrap_or_else(|| PathBuf::from(".local/share"));
    base.join("nucleos-dev")
}

/// `check_id` of `tree_state.py`, tightened for git: letters, digits, `.`, `_` and `-`, starting
/// with a letter or digit, and no `..`, trailing `.` or `.lock`, which a git ref refuses.
fn valid_name(value: &str) -> bool {
    value
        .chars()
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !value.contains("..")
        && !value.ends_with('.')
        && !value.ends_with(".lock")
}

fn check_name(value: &str, what: &str) -> Result<(), OpenError> {
    if valid_name(value) {
        Ok(())
    } else {
        Err(OpenError::BadName(format!(
            "{what} {value:?} must be letters, digits, '.', '_' or '-'"
        )))
    }
}

/// A path as the manifest keeps it: forward slashes, as `project_root` is.
fn path_text(path: &Path) -> String {
    let text = path.to_string_lossy().into_owned();
    if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text
    }
}

pub(super) fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

// ----- types -----

/// What `open_branch` needs from outside the manifest.
#[derive(Debug, Clone)]
pub struct OpenConfig {
    pub dev_home: PathBuf,
    /// The integration branch: the feature branch is made from it, and may not be it.
    pub base_branch: String,
    /// Untracked paths of the project root that a worktree gets a link to.
    pub links: Vec<String>,
    pub free_disk_gb: f64,
    pub floor_gb: f64,
    pub max_live_branches: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Opened {
    pub branch: String,
    pub state: String,
    pub worktree: Option<String>,
    pub git_branch: Option<String>,
    pub target_dir: Option<String>,
    pub skipped_links: Vec<String>,
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bound {
    pub branch: String,
    pub node_id: String,
    pub state: String,
    pub replayed: bool,
}

#[derive(Debug)]
pub enum OpenError {
    JobNotFound(String),
    BranchNotFound(String),
    JobFinished(String),
    BadName(String),
    /// The branch is not in the state this move needs (`queued` to open, `open` to bind).
    NotQueued {
        branch: String,
        state: String,
    },
    BaseNotMerged {
        branch: String,
        base: String,
    },
    NotAdmitted(Vec<String>),
    OverIntegration(String),
    WorktreeExists(PathBuf),
    Git(String),
    Link(String),
    NodeNotFound(String),
    AlreadyBound {
        branch: String,
        node_id: String,
    },
    NodeTaken {
        node_id: String,
        branch: String,
    },
    ForeignNode {
        node_id: String,
    },
    Engine(String),
    Store(StoreError),
    Db(sqlx::Error),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenError::JobNotFound(id) => write!(f, "tree job {id} not found"),
            OpenError::BranchNotFound(name) => write!(f, "branch {name} not found"),
            OpenError::JobFinished(id) => write!(f, "tree job {id} is finished"),
            OpenError::BadName(reason) => write!(f, "{reason}"),
            OpenError::NotQueued { branch, state } => {
                write!(
                    f,
                    "branch {branch} is {state}, which this move does not take"
                )
            }
            OpenError::BaseNotMerged { branch, base } => {
                write!(
                    f,
                    "branch {branch} is based on {base}, which is not merged yet"
                )
            }
            OpenError::NotAdmitted(reasons) => {
                write!(f, "the branch is not admitted: {}", reasons.join("; "))
            }
            OpenError::OverIntegration(branch) => write!(
                f,
                "the feature branch {branch} is the integration branch; work needs a branch of its own"
            ),
            OpenError::WorktreeExists(path) => write!(f, "{} already exists", path.display()),
            OpenError::Git(reason) => write!(f, "{reason}"),
            OpenError::Link(reason) => write!(f, "could not link: {reason}"),
            OpenError::NodeNotFound(id) => write!(f, "work node {id} not found"),
            OpenError::AlreadyBound { branch, node_id } => {
                write!(f, "branch {branch} is already bound to node {node_id}")
            }
            OpenError::NodeTaken { node_id, branch } => {
                write!(f, "node {node_id} is already the node of branch {branch}")
            }
            OpenError::ForeignNode { node_id } => {
                write!(f, "node {node_id} belongs to another tree job")
            }
            OpenError::Engine(reason) => write!(f, "{reason}"),
            OpenError::Store(error) => write!(f, "{error}"),
            OpenError::Db(error) => write!(f, "database error: {error}"),
        }
    }
}

impl std::error::Error for OpenError {}

impl From<StoreError> for OpenError {
    fn from(error: StoreError) -> Self {
        OpenError::Store(error)
    }
}

impl From<sqlx::Error> for OpenError {
    fn from(error: sqlx::Error) -> Self {
        OpenError::Db(error)
    }
}

// ----- git and links -----

/// Runs `git -C <root> <args>` and answers its trimmed stdout, or its stderr as the error.
async fn git(root: &str, args: &[&str]) -> Result<String, String> {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(|error| format!("git {}: {error}", args.join(" ")))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

/// The pattern of the report files a tree node writes in its worktree (`.nucleos-report.md`, and the
/// root's `.nucleos-root-report.md`). A node may only write inside its worktree, so the files cannot
/// live elsewhere; they are kept out of git instead, or `checkpoint` (`git add -A`) would commit
/// them and a node with no other work would look like it has a diff.
const REPORT_EXCLUDE: &str = "/.nucleos-*report.md";

/// Everything a tree worktree holds that is the daemon's and not the branch's, so `checkpoint`
/// (`git add -A`) must not commit it: the report files, the agents' workflow files that
/// `workflow --sync` writes, and the classifier settings a run inherits from the project root.
/// `info/exclude` only affects UNTRACKED files, so a repository that tracks any of these (this one
/// tracks `.claude/hooks/ask_daemon.py`) keeps seeing its changes.
const TREE_EXCLUDES: [&str; 5] = [
    REPORT_EXCLUDE,
    "/.agents/",
    "/AGENTS.md",
    "/.claude/settings.json",
    "/.claude/hooks/ask_daemon.py",
];

/// Adds [`TREE_EXCLUDES`] to the repository's `info/exclude`, which every worktree of the
/// repository shares. Lines that are there already are not repeated.
async fn exclude_reports(root: &str) -> Result<(), String> {
    let reported = git(root, &["rev-parse", "--git-path", "info/exclude"]).await?;
    // Git answers relative to the directory it ran in when the path is not absolute.
    let exclude = Path::new(root).join(reported);
    let mut contents = std::fs::read_to_string(&exclude).unwrap_or_default();
    let missing: Vec<&str> = TREE_EXCLUDES
        .into_iter()
        .filter(|pattern| !contents.lines().any(|line| line.trim() == *pattern))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    if let Some(parent) = exclude.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    if !contents.is_empty() && !contents.ends_with('\n') {
        contents.push('\n');
    }
    for pattern in missing {
        contents.push_str(pattern);
        contents.push('\n');
    }
    std::fs::write(&exclude, contents).map_err(|error| format!("{}: {error}", exclude.display()))
}

/// Removes a link without following it: a junction or a directory symlink goes with `remove_dir`
/// (Windows), a symlink with `remove_file`. The directory it points at is never touched.
fn unlink(link: &Path) -> std::io::Result<()> {
    std::fs::remove_dir(link).or_else(|_| std::fs::remove_file(link))
}

/// A junction on Windows (it needs no privilege), a symlink elsewhere. An existing path at `link`
/// is an error: a tracked directory of the worktree must not be shadowed.
fn make_link(source: &Path, link: &Path) -> Result<(), String> {
    if link.symlink_metadata().is_ok() {
        return Err(format!("{} already exists in the worktree", link.display()));
    }
    if let Some(parent) = link.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("{}: {error}", parent.display()))?;
    }
    #[cfg(windows)]
    {
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link.to_string_lossy().replace('/', "\\"))
            .arg(source.to_string_lossy().replace('/', "\\"))
            .output()
            .map_err(|error| format!("mklink: {error}"))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(format!(
                "mklink {} failed: {}",
                link.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            ))
        }
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(source, link)
            .map_err(|error| format!("{}: {error}", link.display()))
    }
}

/// A link name must stay inside the project: relative, with no `..`.
fn safe_relative(link: &str) -> bool {
    let path = Path::new(link);
    !link.is_empty()
        && path.is_relative()
        && !path.has_root()
        && path
            .components()
            .all(|part| matches!(part, std::path::Component::Normal(_)))
}

/// Undoes a half-made open: links first (without following them), then the worktree, its branch
/// and a feature branch this very call made. Every step is best effort.
async fn roll_back(
    root: &str,
    worktree: &Path,
    links: &[PathBuf],
    git_branch: &str,
    worktree_added: bool,
    created_feature: Option<&str>,
) {
    for link in links {
        if let Err(error) = unlink(link) {
            tracing::warn!(link = %link.display(), %error, "open rollback: link not removed");
        }
    }
    if worktree_added {
        let worktree = path_text(worktree);
        for args in [
            vec!["worktree", "remove", "--force", worktree.as_str()],
            vec!["branch", "-D", git_branch],
        ] {
            if let Err(error) = git(root, &args).await {
                tracing::warn!(%error, "open rollback step failed");
            }
        }
    }
    if let Some(feature) = created_feature
        && let Err(error) = git(root, &["branch", "-D", feature]).await
    {
        tracing::warn!(%error, "open rollback: feature branch not removed");
    }
}

// ----- open -----

fn opened_from(branch: &Branch, replayed: bool, skipped_links: Vec<String>) -> Opened {
    Opened {
        branch: branch.name.clone(),
        state: branch.state.clone(),
        worktree: branch.worktree.clone(),
        git_branch: branch.git_branch.clone(),
        target_dir: branch.target_dir.clone(),
        skipped_links,
        replayed,
    }
}

async fn write_job(pool: &SqlitePool, job: &mut TreeJob) -> Result<(), OpenError> {
    job.updated_at = now();
    store::update(pool, job).await?;
    Ok(())
}

/// Opens `branch_name` of tree job `tree_id`: for a code branch a git worktree on its own git
/// branch, the links the project asks for and a target dir name; for a docs branch only the state.
/// Idempotent: an open branch answers what it has with `replayed`. On any failure the branch stays
/// `queued` and what the call made is undone.
pub async fn open_branch(
    pool: &SqlitePool,
    tree_id: &str,
    branch_name: &str,
    config: &OpenConfig,
) -> Result<Opened, OpenError> {
    check_name(tree_id, "tree id")?;
    check_name(branch_name, "branch name")?;
    let lock = http::job_lock(tree_id);
    let _guard = lock.lock().await;

    let mut job = store::load(pool, tree_id)
        .await?
        .ok_or_else(|| OpenError::JobNotFound(tree_id.to_owned()))?;
    if job.state == JOB_TERMINAL_STATE {
        return Err(OpenError::JobFinished(tree_id.to_owned()));
    }
    let branch = job
        .manifest
        .branches
        .get(branch_name)
        .cloned()
        .ok_or_else(|| OpenError::BranchNotFound(branch_name.to_owned()))?;
    if branch.state == "open" {
        return Ok(opened_from(&branch, true, Vec::new()));
    }
    if branch.state != "queued" {
        return Err(OpenError::NotQueued {
            branch: branch_name.to_owned(),
            state: branch.state,
        });
    }
    if let Some(base) = &branch.base
        && job
            .manifest
            .branches
            .get(base)
            .is_some_and(|base_branch| base_branch.state != "merged")
    {
        return Err(OpenError::BaseNotMerged {
            branch: branch_name.to_owned(),
            base: base.clone(),
        });
    }

    if branch.layer == DOCS_LAYER {
        let docs = job
            .manifest
            .branches
            .get_mut(branch_name)
            .expect("branch was read");
        apply_transition(docs, "open").map_err(|error| OpenError::Engine(error.to_string()))?;
        let opened = opened_from(docs, false, Vec::new());
        write_job(pool, &mut job).await?;
        return Ok(opened);
    }

    let feature = job
        .feature_branch
        .clone()
        .unwrap_or_else(|| job.manifest.feature_branch.clone());
    if feature == config.base_branch {
        return Err(OpenError::OverIntegration(feature));
    }
    let sizes = live_sizes_gb(&job.manifest).await;
    let book = admission_book();
    let admission = branch_admission(
        &book,
        &job.manifest,
        &branch.layer,
        config.free_disk_gb,
        config.floor_gb,
        config.max_live_branches,
        &sizes,
    );
    if !admission.admit {
        return Err(OpenError::NotAdmitted(admission.reasons));
    }

    let root_text = job.manifest.project_root.clone();
    let root = PathBuf::from(&root_text);
    let worktree = worktree_path(&root, tree_id, branch_name);
    if worktree.symlink_metadata().is_ok() {
        return Err(OpenError::WorktreeExists(worktree));
    }
    let git_branch = git_branch_name(tree_id, branch_name);

    // The feature branch is made from the integration branch when it does not exist yet.
    let feature_ref = format!("refs/heads/{feature}");
    let created_feature = if git(
        &root_text,
        &["rev-parse", "--verify", "--quiet", &feature_ref],
    )
    .await
    .is_ok()
    {
        None
    } else {
        git(&root_text, &["branch", &feature, &config.base_branch])
            .await
            .map_err(OpenError::Git)?;
        Some(feature.as_str())
    };
    let added = git(
        &root_text,
        &[
            "worktree",
            "add",
            "-b",
            &git_branch,
            &path_text(&worktree),
            &feature,
        ],
    )
    .await;
    if let Err(error) = added {
        roll_back(
            &root_text,
            &worktree,
            &[],
            &git_branch,
            false,
            created_feature,
        )
        .await;
        return Err(OpenError::Git(error));
    }

    if let Err(error) = exclude_reports(&root_text).await {
        roll_back(
            &root_text,
            &worktree,
            &[],
            &git_branch,
            true,
            created_feature,
        )
        .await;
        return Err(OpenError::Git(error));
    }

    let mut made = Vec::new();
    let mut skipped_links = Vec::new();
    for name in &config.links {
        let source = root.join(name);
        if !safe_relative(name) || !source.exists() {
            skipped_links.push(name.clone());
            continue;
        }
        let link = worktree.join(name);
        let (from, to) = (source, link.clone());
        let linked = tokio::task::spawn_blocking(move || make_link(&from, &to))
            .await
            .unwrap_or_else(|error| Err(format!("link task failed: {error}")));
        match linked {
            Ok(()) => made.push(link),
            Err(reason) => {
                roll_back(
                    &root_text,
                    &worktree,
                    &made,
                    &git_branch,
                    true,
                    created_feature,
                )
                .await;
                return Err(OpenError::Link(reason));
            }
        }
    }

    let target = path_text(&target_dir(&config.dev_home, tree_id, branch_name));
    let code = job
        .manifest
        .branches
        .get_mut(branch_name)
        .expect("branch was read");
    if let Err(error) = apply_transition(code, "open") {
        roll_back(
            &root_text,
            &worktree,
            &made,
            &git_branch,
            true,
            created_feature,
        )
        .await;
        return Err(OpenError::Engine(error.to_string()));
    }
    code.worktree = Some(path_text(&worktree));
    code.git_branch = Some(git_branch.clone());
    code.target_dir = Some(target);
    let opened = opened_from(code, false, skipped_links);
    if let Err(error) = write_job(pool, &mut job).await {
        roll_back(
            &root_text,
            &worktree,
            &made,
            &git_branch,
            true,
            created_feature,
        )
        .await;
        return Err(error);
    }
    Ok(opened)
}

// ----- bind -----

/// Binds node `node_id` to `branch_name` and moves the branch to `building`. Idempotent for the
/// same node; a branch bound to another node, or a node bound to another branch, is refused.
pub async fn bind_branch(
    pool: &SqlitePool,
    tree_id: &str,
    branch_name: &str,
    node_id: &str,
) -> Result<Bound, OpenError> {
    let lock = http::job_lock(tree_id);
    let _guard = lock.lock().await;

    let mut job = store::load(pool, tree_id)
        .await?
        .ok_or_else(|| OpenError::JobNotFound(tree_id.to_owned()))?;
    if job.state == JOB_TERMINAL_STATE {
        return Err(OpenError::JobFinished(tree_id.to_owned()));
    }
    let branch = job
        .manifest
        .branches
        .get(branch_name)
        .cloned()
        .ok_or_else(|| OpenError::BranchNotFound(branch_name.to_owned()))?;
    if let Some(bound) = &branch.node_id {
        if bound == node_id {
            return Ok(Bound {
                branch: branch_name.to_owned(),
                node_id: node_id.to_owned(),
                state: branch.state,
                replayed: true,
            });
        }
        return Err(OpenError::AlreadyBound {
            branch: branch_name.to_owned(),
            node_id: bound.clone(),
        });
    }
    let bindable = if branch.layer == DOCS_LAYER {
        matches!(branch.state.as_str(), "queued" | "open")
    } else {
        branch.state == "open"
    };
    if !bindable {
        return Err(OpenError::NotQueued {
            branch: branch_name.to_owned(),
            state: branch.state,
        });
    }

    let row: Option<(String, Option<String>)> =
        sqlx::query_as("SELECT root_id, tree_job_id FROM work_nodes WHERE id = ?")
            .bind(node_id)
            .fetch_optional(pool)
            .await?;
    let Some((root_id, linked_job)) = row else {
        return Err(OpenError::NodeNotFound(node_id.to_owned()));
    };
    let other_root: Option<String> =
        sqlx::query_scalar("SELECT id FROM tree_jobs WHERE root_node_id = ? AND id != ?")
            .bind(&root_id)
            .bind(tree_id)
            .fetch_optional(pool)
            .await?;
    if other_root.is_some()
        || linked_job
            .as_deref()
            .is_some_and(|linked| linked != tree_id)
    {
        return Err(OpenError::ForeignNode {
            node_id: node_id.to_owned(),
        });
    }
    if let Some(other) = job
        .manifest
        .branches
        .values()
        .find(|other| other.node_id.as_deref() == Some(node_id))
    {
        return Err(OpenError::NodeTaken {
            node_id: node_id.to_owned(),
            branch: other.name.clone(),
        });
    }

    sqlx::query("UPDATE work_nodes SET tree_job_id = ? WHERE id = ? AND tree_job_id IS NULL")
        .bind(tree_id)
        .bind(node_id)
        .execute(pool)
        .await?;
    let bound = job
        .manifest
        .branches
        .get_mut(branch_name)
        .expect("branch was read");
    apply_transition(bound, "building").map_err(|error| OpenError::Engine(error.to_string()))?;
    bound.node_id = Some(node_id.to_owned());
    let state = bound.state.clone();
    write_job(pool, &mut job).await?;
    Ok(Bound {
        branch: branch_name.to_owned(),
        node_id: node_id.to_owned(),
        state,
        replayed: false,
    })
}

// The stored launches and the relaunches built on them live in `relaunch`; they are re-exported here
// because this is where callers have always imported them from.
pub use super::relaunch::{
    Answered, Relaunched, StoredLaunch, answer_and_relaunch, latest_launch, relaunch_resumed,
    save_launch, stored_launch,
};
pub(super) use super::relaunch::{
    DIED_PREFIX, FIX_PREFIX, WRAP_PREFIX, relaunch_node, run_is_live,
};

// ----- routes -----

type Refusal = (StatusCode, Json<Value>);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/trees/{id}/branches/{name}/open", post(post_open))
        .route("/trees/{id}/branches/{name}/bind", post(post_bind))
}

fn refusal(error: OpenError) -> Refusal {
    let status = match &error {
        OpenError::JobNotFound(_) | OpenError::BranchNotFound(_) | OpenError::NodeNotFound(_) => {
            StatusCode::NOT_FOUND
        }
        OpenError::BadName(_) => StatusCode::UNPROCESSABLE_ENTITY,
        OpenError::Git(_) | OpenError::Link(_) | OpenError::Store(_) | OpenError::Db(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
        _ => StatusCode::CONFLICT,
    };
    if let OpenError::NotAdmitted(reasons) = &error {
        return (
            status,
            Json(json!({ "error": error.to_string(), "reasons": reasons })),
        );
    }
    if status == StatusCode::INTERNAL_SERVER_ERROR {
        tracing::warn!(%error, "tree branch move failed");
    }
    refuse(status, error.to_string())
}

/// `tree:` of the project's `speed.yaml`: the integration branch and the links. The integration
/// branch defaults to the branch the main checkout is on, then to `master`. The `tree:` block of the
/// project's own `project.yaml` lives in the state dir, outside the root, and is not read here.
async fn project_tree_config(root: &str) -> (String, Vec<String>) {
    let doc = std::fs::read_to_string(Path::new(root).join(".nucleos/workflow/speed.yaml"))
        .ok()
        .and_then(|text| serde_yaml::from_str::<serde_yaml::Value>(&text).ok());
    let tree = doc.as_ref().and_then(|doc| doc.get("tree"));
    let links = tree
        .and_then(|tree| tree.get("links"))
        .and_then(serde_yaml::Value::as_sequence)
        .map(|links| {
            links
                .iter()
                .filter_map(|link| link.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let configured = tree
        .and_then(|tree| tree.get("base_branch"))
        .and_then(serde_yaml::Value::as_str)
        .map(str::to_owned);
    let base = match configured {
        Some(base) => base,
        None => git(root, &["symbolic-ref", "--short", "HEAD"])
            .await
            .ok()
            .filter(|branch| !branch.is_empty())
            .unwrap_or_else(|| "master".to_owned()),
    };
    (base, links)
}

/// The `OpenConfig` the open route builds for a project: `speed.yaml`'s integration branch and
/// links, the tree limits and the free disk. Shared with the root start.
pub(super) async fn open_config(root: &str) -> OpenConfig {
    let (floor_gb, max_live_branches) = tree_limits(root);
    let free_disk_gb = crate::health::available_space(Path::new(root))
        .map(|bytes| bytes as f64 / BYTES_PER_GB)
        .unwrap_or(0.0);
    let (base_branch, links) = project_tree_config(root).await;
    OpenConfig {
        dev_home: dev_home(),
        base_branch,
        links,
        free_disk_gb,
        floor_gb,
        max_live_branches,
    }
}

/// Puts the project's pinned workflow in the new worktree: `nucleos-core --workflow-sync <tree>`.
/// The answer is for the caller to read; a failure never fails the open. A test build does not
/// start a process: its executable is the test harness, not the daemon.
pub(super) async fn workflow_sync(worktree: &str) -> String {
    if cfg!(test) {
        return "skipped: test build".to_owned();
    }
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(error) => return format!("failed: {error}"),
    };
    let output = tokio::process::Command::new(exe)
        .arg("--workflow-sync")
        .arg(worktree)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await;
    match output {
        Ok(output) if output.status.success() => "synced".to_owned(),
        Ok(output) => format!("failed: {}", String::from_utf8_lossy(&output.stderr).trim()),
        Err(error) => format!("failed: {error}"),
    }
}

async fn post_open(
    State(state): State<AppState>,
    UrlPath((id, name)): UrlPath<(String, String)>,
    body: Bytes,
) -> Result<Json<Value>, Refusal> {
    // The body is optional; when there is one it must be JSON, and nothing in it is read.
    if !body.iter().all(u8::is_ascii_whitespace)
        && let Err(error) = serde_json::from_slice::<Value>(&body)
    {
        return Err(refuse(StatusCode::BAD_REQUEST, error.to_string()));
    }
    check_name(&id, "tree id").map_err(refusal)?;
    check_name(&name, "branch name").map_err(refusal)?;
    let job = store::load(&state.pool, &id)
        .await
        .map_err(|error| refusal(error.into()))?
        .ok_or_else(|| refusal(OpenError::JobNotFound(id.clone())))?;
    let config = open_config(&job.manifest.project_root).await;

    let opened = open_branch(&state.pool, &id, &name, &config)
        .await
        .map_err(refusal)?;
    let sync = match (&opened.worktree, opened.replayed) {
        (Some(worktree), false) => workflow_sync(worktree).await,
        (_, true) => "skipped: already open".to_owned(),
        (None, false) => "skipped: no worktree".to_owned(),
    };
    let env = match &opened.target_dir {
        Some(target) => json!({ "CARGO_TARGET_DIR": target }),
        None => json!({}),
    };
    Ok(Json(json!({
        "branch": opened.branch,
        "state": opened.state,
        "worktree": opened.worktree,
        "git_branch": opened.git_branch,
        "target_dir": opened.target_dir,
        "env": env,
        "skipped_links": opened.skipped_links,
        "workflow_sync": sync,
        "replayed": opened.replayed,
    })))
}

#[derive(Debug, Deserialize)]
struct BindBody {
    node_id: Option<String>,
}

async fn post_bind(
    State(state): State<AppState>,
    UrlPath((id, name)): UrlPath<(String, String)>,
    body: Bytes,
) -> Result<Json<Value>, Refusal> {
    let body: BindBody = serde_json::from_slice(&body)
        .map_err(|error| refuse(StatusCode::BAD_REQUEST, error.to_string()))?;
    let node_id = body
        .node_id
        .filter(|node| !node.trim().is_empty())
        .ok_or_else(|| refuse(StatusCode::BAD_REQUEST, "node_id is required"))?;
    let bound = bind_branch(&state.pool, &id, &name, &node_id)
        .await
        .map_err(refusal)?;
    Ok(Json(json!({
        "name": bound.branch,
        "node_id": bound.node_id,
        "state": bound.state,
        "replayed": bound.replayed,
    })))
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Mutex;
    use std::time::Duration;

    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::{Value, json};
    use sqlx::SqlitePool;
    use tower::ServiceExt;

    use super::*;
    use crate::execution::git_exec::testkit::{initialize_repo, space_free_tempdir};
    use crate::state::AppState;
    use crate::tree_job::commands::{self, CommandRun, Invocation, RunOutcome};
    use crate::tree_job::decisions;
    use crate::tree_job::http;
    use crate::tree_job::model::{Branch, Manifest};
    use crate::tree_job::node_run;
    use crate::tree_job::store::{self, StoreError, TreeJob};

    const CREATED: &str = "2026-10-30T10:00:00+00:00";
    const TREE: &str = "job-1";
    const ROOT: &str = "job-1-root";
    const OTHER_TREE: &str = "job-2";
    const OTHER_ROOT: &str = "job-2-root";
    const PROJECT: &str = "p1";
    const FEATURE: &str = "feat/tree";
    const INTEGRATION: &str = "master";
    const BRANCH: &str = "api";
    const OTHER_BRANCH: &str = "ui";
    const NODE: &str = "node-api";
    const OTHER_NODE: &str = "node-ui";
    const KEY: &str = "key-1";
    const OTHER_KEY: &str = "key-2";
    const PROMPT: &str = "do it";
    const QUESTION: &str = "pick A or B";
    const ANSWER: &str = "Pick A";
    const CHANNEL: &str = "telegram";
    /// For a job whose test never reaches the disk.
    const NOWHERE: &str = "C:/no-such-root";
    /// High enough not to collide with an id the fake agent's runs get.
    const DEAD_RUN: i64 = 4711;
    const LIVE_RUN: i64 = 4712;
    /// `create.rs`: worktree mode with no project and no cwd fails before any run row exists.
    const REFUSED_WITHOUT_A_PROJECT: &str = "worktree";
    const CORRUPT_KIND: &str = "work_tree_corrupt";
    const RELAUNCH_SKIPPED_KIND: &str = "work_tree_relaunch_skipped";

    // ----- fixtures: paths and git -----

    fn fwd(path: impl AsRef<Path>) -> String {
        path.as_ref().to_string_lossy().replace('\\', "/")
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("git should start");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// A commit on whatever `dir` has checked out.
    fn commit_file(dir: &Path, relative: &str, text: &str) {
        let file = dir.join(relative);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, text).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", relative]);
    }

    /// A temp repository with one commit on `master`, and a dev home beside it.
    struct Checkout {
        _container: tempfile::TempDir,
        repo: PathBuf,
        dev_home: PathBuf,
    }

    impl Checkout {
        fn new() -> Checkout {
            let container = space_free_tempdir("tree-open-");
            let repo = container.path().join("repo");
            initialize_repo(&repo);
            // `initialize_repo` takes the machine's `init.defaultBranch`.
            git(&repo, &["branch", "-M", INTEGRATION]);
            let dev_home = container.path().join("dev-home");
            std::fs::create_dir_all(&dev_home).unwrap();
            Checkout {
                _container: container,
                repo,
                dev_home,
            }
        }

        /// A checkout whose `speed.yaml` sets this floor, for the routes, which read their limits
        /// from the project.
        fn with_floor(floor_gb: &str) -> Checkout {
            let checkout = Checkout::new();
            let dir = checkout.repo.join(".nucleos/workflow");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join("speed.yaml"),
                format!("tree:\n  floor_gb: {floor_gb}\n"),
            )
            .unwrap();
            checkout
        }

        fn root(&self) -> String {
            fwd(&self.repo)
        }

        fn config(&self) -> OpenConfig {
            OpenConfig {
                dev_home: self.dev_home.clone(),
                base_branch: INTEGRATION.to_owned(),
                links: Vec::new(),
                free_disk_gb: 10_000.0,
                floor_gb: 0.0,
                max_live_branches: 4,
            }
        }

        /// The feature branch with one commit past `master`; answers its tip. `master` stays out.
        fn feature_past_master(&self) -> String {
            git(&self.repo, &["checkout", "-q", "-b", FEATURE]);
            commit_file(&self.repo, "feature.txt", "feature\n");
            let tip = self.tip(FEATURE);
            git(&self.repo, &["checkout", "-q", INTEGRATION]);
            tip
        }

        fn tip(&self, reference: &str) -> String {
            git(&self.repo, &["rev-parse", reference])
        }

        fn has_ref(&self, branch: &str) -> bool {
            !git(
                &self.repo,
                &["for-each-ref", &format!("refs/heads/{branch}")],
            )
            .is_empty()
        }

        fn tree_refs(&self) -> String {
            git(&self.repo, &["for-each-ref", "refs/heads/tree/"])
        }

        /// Entries under `.claude/worktrees`; none when the directory was never made.
        fn worktree_dirs(&self) -> usize {
            std::fs::read_dir(self.repo.join(WORKTREES_DIR))
                .map(|entries| entries.count())
                .unwrap_or(0)
        }

        /// What git knows of, the main checkout included.
        fn registered_worktrees(&self) -> usize {
            git(&self.repo, &["worktree", "list", "--porcelain"])
                .lines()
                .filter(|line| line.starts_with("worktree "))
                .count()
        }

        fn assert_nothing_made(&self) {
            assert_eq!(self.worktree_dirs(), 0, "a worktree directory was made");
            assert_eq!(self.registered_worktrees(), 1, "a worktree was registered");
            assert_eq!(self.tree_refs(), "", "a tree branch was made");
        }
    }

    // ----- fixtures: jobs and rows -----

    fn code(name: &str, state: &str) -> Branch {
        let mut branch = Branch::new(name, &[format!("core/src/{name}/**")], "core", None);
        branch.state = state.to_owned();
        branch
    }

    fn docs(name: &str, state: &str) -> Branch {
        let mut branch = Branch::new(name, &[format!(".ai/plans/{name}.md")], "docs", None);
        branch.state = state.to_owned();
        branch
    }

    fn based_on(mut branch: Branch, base: &str) -> Branch {
        branch.base = Some(base.to_owned());
        branch
    }

    fn with_node(mut branch: Branch, node_id: &str) -> Branch {
        branch.node_id = Some(node_id.to_owned());
        branch
    }

    fn job_of(id: &str, project_root: &str, feature: &str, branches: Vec<Branch>) -> TreeJob {
        let mut manifest = Manifest::new(id, feature, "Ship it", project_root, &[]);
        for branch in branches {
            manifest.branches.insert(branch.name.clone(), branch);
        }
        TreeJob {
            id: id.to_owned(),
            root_node_id: format!("{id}-root"),
            project_id: Some(PROJECT.to_owned()),
            feature_branch: Some(feature.to_owned()),
            goal: "Ship it".to_owned(),
            state: "open".to_owned(),
            manifest,
            contract: None,
            created_at: CREATED.to_owned(),
            updated_at: CREATED.to_owned(),
        }
    }

    /// Stores the job `TREE` over `checkout`, on the usual feature branch.
    async fn seed_on(pool: &SqlitePool, checkout: &Checkout, branches: Vec<Branch>) {
        let job = job_of(TREE, &checkout.root(), FEATURE, branches);
        store::save(pool, &job).await.unwrap();
    }

    /// The same for a route: the `core` layer costs nothing, so no real free disk is needed.
    async fn seed_for_route(pool: &SqlitePool, checkout: &Checkout, branches: Vec<Branch>) {
        let mut job = job_of(TREE, &checkout.root(), FEATURE, branches);
        job.manifest.costs_gb.insert("core".to_owned(), json!(0.0));
        store::save(pool, &job).await.unwrap();
    }

    /// Stores the job `TREE` for a test that never reaches git.
    async fn seed_elsewhere(pool: &SqlitePool, branches: Vec<Branch>) {
        let job = job_of(TREE, NOWHERE, FEATURE, branches);
        store::save(pool, &job).await.unwrap();
    }

    async fn finish_job(pool: &SqlitePool, id: &str) {
        let mut job = store::load(pool, id).await.unwrap().unwrap();
        job.state = "finished".to_owned();
        job.manifest.state = "finished".to_owned();
        store::update(pool, &job).await.unwrap();
    }

    async fn branch_of(pool: &SqlitePool, name: &str) -> Branch {
        store::load(pool, TREE)
            .await
            .unwrap()
            .unwrap()
            .manifest
            .branches[name]
            .clone()
    }

    async fn assert_still_queued(pool: &SqlitePool, name: &str) {
        let branch = branch_of(pool, name).await;
        assert_eq!(branch.state, "queued");
        assert_eq!(branch.worktree, None);
        assert_eq!(branch.git_branch, None);
        assert_eq!(branch.target_dir, None);
    }

    async fn seed_node_row(
        pool: &SqlitePool,
        id: &str,
        root_id: &str,
        tree_job_id: Option<&str>,
        run_id: Option<i64>,
    ) {
        sqlx::query(
            "INSERT INTO work_nodes (id, root_id, depth, kind, title, goal, status, run_id,
                                     tree_job_id, created_at, updated_at)
             VALUES (?, ?, 1, 'orchestrator', 'title', 'goal', 'running', ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(root_id)
        .bind(run_id.map(|run_id| run_id.to_string()))
        .bind(tree_job_id)
        .bind(CREATED)
        .bind(CREATED)
        .execute(pool)
        .await
        .unwrap();
    }

    /// A node as `/nodes` leaves it: under the job's root and not linked to the job yet.
    async fn seed_unlinked_node(pool: &SqlitePool, id: &str) {
        seed_node_row(pool, id, ROOT, None, None).await;
    }

    /// A running node of the job `TREE`.
    async fn seed_linked_node(pool: &SqlitePool, id: &str, run_id: Option<i64>) {
        seed_node_row(pool, id, ROOT, Some(TREE), run_id).await;
    }

    async fn seed_run(pool: &SqlitePool, id: i64, status: &str) {
        sqlx::query("INSERT INTO runs (id, prompt, status, created_at) VALUES (?, 'node', ?, ?)")
            .bind(id)
            .bind(status)
            .bind(CREATED)
            .execute(pool)
            .await
            .unwrap();
    }

    /// What a `launch` leaves behind when it dies after reserving its key.
    async fn reserve(pool: &SqlitePool, node_id: &str, key: &str) {
        node_run::apply(pool, node_id, "launch", key, "")
            .await
            .unwrap();
    }

    fn launch_of(node_id: &str, key: &str, mode: &str) -> StoredLaunch {
        StoredLaunch {
            node_id: node_id.to_owned(),
            key: key.to_owned(),
            prompt: PROMPT.to_owned(),
            project_id: None,
            cwd: None,
            mode: mode.to_owned(),
        }
    }

    async fn save_shadow_launch(pool: &SqlitePool, node_id: &str, key: &str) {
        save_launch(pool, &launch_of(node_id, key, "shadow"))
            .await
            .unwrap();
    }

    async fn stored_result(
        pool: &SqlitePool,
        node_id: &str,
        transition: &str,
        key: &str,
    ) -> Option<String> {
        let stored: Option<Option<String>> = sqlx::query_scalar(
            "SELECT result FROM tree_transitions WHERE node_id = ? AND transition = ? AND key = ?",
        )
        .bind(node_id)
        .bind(transition)
        .bind(key)
        .fetch_optional(pool)
        .await
        .unwrap();
        stored.flatten()
    }

    async fn run_rows(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn run_prompt(pool: &SqlitePool, run_id: i64) -> String {
        sqlx::query_scalar("SELECT prompt FROM runs WHERE id = ?")
            .bind(run_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn node_status(pool: &SqlitePool, id: &str) -> String {
        sqlx::query_scalar("SELECT status FROM work_nodes WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn node_run_id(pool: &SqlitePool, id: &str) -> Option<String> {
        sqlx::query_scalar("SELECT run_id FROM work_nodes WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn node_tree_job(pool: &SqlitePool, id: &str) -> Option<String> {
        sqlx::query_scalar("SELECT tree_job_id FROM work_nodes WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn feed_summaries(pool: &SqlitePool, kind: &str) -> Vec<String> {
        sqlx::query_scalar("SELECT summary FROM feed WHERE kind = ?")
            .bind(kind)
            .fetch_all(pool)
            .await
            .unwrap()
    }

    /// A state whose fake agent stays alive far longer than any test here, so that a run this file
    /// starts still counts as live when a test asks again.
    async fn live_agent_state() -> AppState {
        let (state, runner) = crate::execution::runs::testkit::test_state_with_runner(
            Some(Duration::from_secs(60)),
            Duration::from_secs(600),
        )
        .await;
        runner.canned.lock().unwrap().as_mut().unwrap().session_id = None;
        state
    }

    /// Asserts a relaunch pass started nothing and left the run rows at `rows`.
    async fn assert_started_nothing(pool: &SqlitePool, relaunched: &Relaunched, rows: i64) {
        assert!(relaunched.started.is_empty(), "{relaunched:?}");
        assert_eq!(run_rows(pool).await, rows);
    }

    // ----- fixtures: routes -----

    fn app(state: &AppState) -> Router {
        routes().with_state(state.clone())
    }

    fn tree_app(state: &AppState) -> Router {
        http::routes().with_state(state.clone())
    }

    fn both_apps(state: &AppState) -> Router {
        http::routes().merge(routes()).with_state(state.clone())
    }

    async fn send(app: &Router, uri: &str, body: Body) -> (StatusCode, Value) {
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .header("Authorization", "Bearer test-token")
            .header("Content-Type", "application/json")
            .body(body)
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, value)
    }

    async fn post(app: &Router, uri: &str, body: Value) -> (StatusCode, Value) {
        send(app, uri, Body::from(body.to_string())).await
    }

    fn open_uri(tree: &str, branch: &str) -> String {
        format!("/trees/{tree}/branches/{branch}/open")
    }

    fn bind_uri(tree: &str, branch: &str) -> String {
        format!("/trees/{tree}/branches/{branch}/bind")
    }

    fn transition_uri(tree: &str, node: &str) -> String {
        format!("/trees/{tree}/nodes/{node}/transition")
    }

    fn assert_error(response: &(StatusCode, Value), status: StatusCode) {
        assert_eq!(response.0, status, "{:?}", response.1);
        assert!(
            response.1["error"].is_string(),
            "no error text: {:?}",
            response.1
        );
    }

    // ----- pure -----

    #[test]
    fn worktree_path_is_under_claude_worktrees_named_tree_dash_branch() {
        assert_eq!(WORKTREES_DIR, ".claude/worktrees");
        assert_eq!(
            fwd(worktree_path(Path::new("/root/repo"), TREE, BRANCH)),
            "/root/repo/.claude/worktrees/job-1-api"
        );
    }

    #[test]
    fn target_dir_is_in_the_dev_home_cargo_target_pool() {
        assert_eq!(
            fwd(target_dir(Path::new("/dev/home"), TREE, BRANCH)),
            "/dev/home/cargo-target/job-1-api"
        );
    }

    #[test]
    fn git_branch_name_is_tree_slash_id_slash_name() {
        assert_eq!(git_branch_name(TREE, BRANCH), "tree/job-1/api");
    }

    // ----- open_branch -----

    #[tokio::test]
    async fn open_makes_a_worktree_on_its_own_branch_from_the_feature_tip() {
        let pool = crate::testdb::fresh_pool().await;
        let checkout = Checkout::new();
        let tip = checkout.feature_past_master();
        seed_on(&pool, &checkout, vec![code(BRANCH, "queued")]).await;

        let opened = open_branch(&pool, TREE, BRANCH, &checkout.config())
            .await
            .unwrap();

        assert_eq!(opened.branch, BRANCH);
        assert_eq!(opened.state, "open");
        assert!(!opened.replayed);
        let worktree = PathBuf::from(opened.worktree.as_deref().unwrap());
        assert_eq!(
            fwd(&worktree),
            fwd(worktree_path(&checkout.repo, TREE, BRANCH))
        );
        assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), tip);
        assert_eq!(
            git(&worktree, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "tree/job-1/api"
        );
        assert_eq!(opened.git_branch.as_deref(), Some("tree/job-1/api"));
    }

    #[tokio::test]
    async fn open_creates_a_missing_feature_branch_from_base_and_master_does_not_move() {
        let pool = crate::testdb::fresh_pool().await;
        let checkout = Checkout::new();
        let master = checkout.tip(INTEGRATION);
        assert!(!checkout.has_ref(FEATURE));
        seed_on(&pool, &checkout, vec![code(BRANCH, "queued")]).await;

        let opened = open_branch(&pool, TREE, BRANCH, &checkout.config())
            .await
            .unwrap();

        assert_eq!(checkout.tip(FEATURE), master);
        assert_eq!(checkout.tip(INTEGRATION), master);
        let worktree = PathBuf::from(opened.worktree.as_deref().unwrap());
        assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), master);
    }

    #[tokio::test]
    async fn open_writes_state_worktree_git_branch_and_target_dir_to_the_manifest() {
        let pool = crate::testdb::fresh_pool().await;
        let checkout = Checkout::new();
        checkout.feature_past_master();
        seed_on(&pool, &checkout, vec![code(BRANCH, "queued")]).await;

        let opened = open_branch(&pool, TREE, BRANCH, &checkout.config())
            .await
            .unwrap();

        let branch = branch_of(&pool, BRANCH).await;
        assert_eq!(branch.state, "open");
        assert_eq!(branch.worktree, opened.worktree);
        assert_eq!(branch.git_branch.as_deref(), Some("tree/job-1/api"));
        assert_eq!(branch.target_dir, opened.target_dir);
        assert_eq!(
            fwd(branch.worktree.unwrap()),
            fwd(worktree_path(&checkout.repo, TREE, BRANCH))
        );
        assert_eq!(
            fwd(branch.target_dir.unwrap()),
            fwd(target_dir(&checkout.dev_home, TREE, BRANCH))
        );
    }

    #[tokio::test]
    async fn open_leaves_the_main_checkout_on_its_branch() {
        let pool = crate::testdb::fresh_pool().await;
        let checkout = Checkout::new();
        git(&checkout.repo, &["checkout", "-q", "-b", "owner-work"]);
        seed_on(&pool, &checkout, vec![code(BRANCH, "queued")]).await;

        open_branch(&pool, TREE, BRANCH, &checkout.config())
            .await
            .unwrap();

        assert_eq!(
            git(&checkout.repo, &["rev-parse", "--abbrev-ref", "HEAD"]),
            "owner-work"
        );
    }

    /// `tree/...` from `master` would have the branch's work land directly on the integration
    /// branch.
    #[tokio::test]
    async fn open_refuses_a_feature_branch_that_is_the_integration_branch_and_creates_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let checkout = Checkout::new();
        let job = job_of(
            TREE,
            &checkout.root(),
            INTEGRATION,
            vec![code(BRANCH, "queued")],
        );
        store::save(&pool, &job).await.unwrap();

        let error = open_branch(&pool, TREE, BRANCH, &checkout.config())
            .await
            .unwrap_err();

        assert!(matches!(error, OpenError::OverIntegration(_)), "{error:?}");
        checkout.assert_nothing_made();
        assert_still_queued(&pool, BRANCH).await;
    }

    /// A docs branch works in the feature checkout: nothing is allocated, so neither the disk nor
    /// the integration rule has a say.
    #[tokio::test]
    async fn open_of_a_docs_branch_allocates_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let checkout = Checkout::new();
        let job = job_of(
            TREE,
            &checkout.root(),
            INTEGRATION,
            vec![docs(BRANCH, "queued")],
        );
        store::save(&pool, &job).await.unwrap();
        let config = OpenConfig {
            free_disk_gb: 0.0,
            floor_gb: 1e12,
            ..checkout.config()
        };

        let opened = open_branch(&pool, TREE, BRANCH, &config).await.unwrap();

        assert_eq!(
            opened,
            Opened {
                branch: BRANCH.to_owned(),
                state: "open".to_owned(),
                worktree: None,
                git_branch: None,
                target_dir: None,
                skipped_links: Vec::new(),
                replayed: false,
            }
        );
        checkout.assert_nothing_made();
        let branch = branch_of(&pool, BRANCH).await;
        assert_eq!(branch.state, "open");
        assert_eq!(branch.worktree, None);
        assert_eq!(branch.git_branch, None);
        assert_eq!(branch.target_dir, None);
    }

    /// The name becomes a path segment and a ref segment, so a stored bad one is refused too.
    #[tokio::test]
    async fn open_refuses_a_bad_branch_name_and_creates_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let checkout = Checkout::new();
        let names = ["../evil", "a/b", "a\\b", ".hidden", "-x", "a b"];
        let branches = names.iter().map(|name| code(name, "queued")).collect();
        seed_on(&pool, &checkout, branches).await;

        for name in names {
            let error = open_branch(&pool, TREE, name, &checkout.config())
                .await
                .unwrap_err();

            assert!(matches!(error, OpenError::BadName(_)), "{name}: {error:?}");
            assert_still_queued(&pool, name).await;
        }
        checkout.assert_nothing_made();
        assert!(!checkout.has_ref(FEATURE));
    }

    #[tokio::test]
    async fn open_refuses_a_bad_tree_id_and_creates_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let checkout = Checkout::new();

        for id in ["../evil", "a/b", ".hidden", "-x", "a b"] {
            let job = job_of(id, &checkout.root(), FEATURE, vec![code(BRANCH, "queued")]);
            store::save(&pool, &job).await.unwrap();

            let error = open_branch(&pool, id, BRANCH, &checkout.config())
                .await
                .unwrap_err();

            assert!(matches!(error, OpenError::BadName(_)), "{id}: {error:?}");
        }
        checkout.assert_nothing_made();
        assert!(!checkout.has_ref(FEATURE));
    }

    #[tokio::test]
    async fn open_of_an_unknown_job_or_branch_is_not_found() {
        let pool = crate::testdb::fresh_pool().await;
        let checkout = Checkout::new();
        seed_on(&pool, &checkout, vec![code(BRANCH, "queued")]).await;

        let error = open_branch(&pool, "no-such-job", BRANCH, &checkout.config())
            .await
            .unwrap_err();
        assert!(matches!(error, OpenError::JobNotFound(_)), "{error:?}");

        let error = open_branch(&pool, TREE, "no-such-branch", &checkout.config())
            .await
            .unwrap_err();
        assert!(matches!(error, OpenError::BranchNotFound(_)), "{error:?}");

        checkout.assert_nothing_made();
    }

    #[tokio::test]
    async fn open_refuses_a_finished_job() {
        let pool = crate::testdb::fresh_pool().await;
        let checkout = Checkout::new();
        seed_on(&pool, &checkout, vec![code(BRANCH, "queued")]).await;
        finish_job(&pool, TREE).await;

        let error = open_branch(&pool, TREE, BRANCH, &checkout.config())
            .await
            .unwrap_err();

        assert!(matches!(error, OpenError::JobFinished(_)), "{error:?}");
        checkout.assert_nothing_made();
        assert_still_queued(&pool, BRANCH).await;
    }

    #[tokio::test]
    async fn open_refuses_a_branch_whose_base_is_not_merged() {
        let pool = crate::testdb::fresh_pool().await;
        let checkout = Checkout::new();
        let branches = vec![
            based_on(code(BRANCH, "queued"), OTHER_BRANCH),
            code(OTHER_BRANCH, "building"),
        ];
        seed_on(&pool, &checkout, branches).await;

        let error = open_branch(&pool, TREE, BRANCH, &checkout.config())
            .await
            .unwrap_err();

        match error {
            OpenError::BaseNotMerged { branch, base } => {
                assert_eq!(branch, BRANCH);
                assert_eq!(base, OTHER_BRANCH);
            }
            other => panic!("expected BaseNotMerged, got {other:?}"),
        }
        checkout.assert_nothing_made();
        assert_still_queued(&pool, BRANCH).await;
    }

    #[tokio::test]
    async fn open_takes_a_branch_whose_base_is_merged() {
        let pool = crate::testdb::fresh_pool().await;
        let checkout = Checkout::new();
        let branches = vec![
            based_on(code(BRANCH, "queued"), OTHER_BRANCH),
            code(OTHER_BRANCH, "merged"),
        ];
        seed_on(&pool, &checkout, branches).await;

        let opened = open_branch(&pool, TREE, BRANCH, &checkout.config())
            .await
            .unwrap();

        assert_eq!(opened.state, "open");
        assert!(PathBuf::from(opened.worktree.unwrap()).is_dir());
    }

    #[tokio::test]
    async fn open_below_the_floor_is_not_admitted_and_changes_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        let checkout = Checkout::new();
        seed_on(&pool, &checkout, vec![code(BRANCH, "queued")]).await;
        let config = OpenConfig {
            free_disk_gb: 1.0,
            floor_gb: 1000.0,
            ..checkout.config()
        };

        let error = open_branch(&pool, TREE, BRANCH, &config).await.unwrap_err();

        match error {
            OpenError::NotAdmitted(reasons) => assert!(!reasons.is_empty()),
            other => panic!("expected NotAdmitted, got {other:?}"),
        }
        checkout.assert_nothing_made();
        assert!(!checkout.has_ref(FEATURE));
        assert_still_queued(&pool, BRANCH).await;
    }

    #[tokio::test]
    async fn open_at_the_cap_is_not_admitted() {
        let pool = crate::testdb::fresh_pool().await;
        let checkout = Checkout::new();
        let branches = vec![
            code(BRANCH, "queued"),
            code("live-1", "building"),
            code("live-2", "open"),
            code("live-3", "gating"),
            code("live-4", "fixing"),
        ];
        seed_on(&pool, &checkout, branches).await;

        let error = open_branch(&pool, TREE, BRANCH, &checkout.config())
            .await
            .unwrap_err();

        match error {
            OpenError::NotAdmitted(reasons) => assert!(!reasons.is_empty()),
            other => panic!("expected NotAdmitted, got {other:?}"),
        }
        checkout.assert_nothing_made();
        assert_still_queued(&pool, BRANCH).await;

        // The cap alone was what refused it.
        let wider = OpenConfig {
            max_live_branches: 5,
            ..checkout.config()
        };
        open_branch(&pool, TREE, BRANCH, &wider).await.unwrap();
    }

    #[tokio::test]
    async fn a_second_open_answers_replayed_with_the_same_worktree() {
        let pool = crate::testdb::fresh_pool().await;
        let checkout = Checkout::new();
        seed_on(&pool, &checkout, vec![code(BRANCH, "queued")]).await;

        let first = open_branch(&pool, TREE, BRANCH, &checkout.config())
            .await
            .unwrap();
        let second = open_branch(&pool, TREE, BRANCH, &checkout.config())
            .await
            .unwrap();

        assert!(!first.replayed);
        assert!(second.replayed);
        assert_eq!(second.state, "open");
        assert_eq!(second.worktree, first.worktree);
        assert_eq!(second.git_branch, first.git_branch);
        assert_eq!(second.target_dir, first.target_dir);
        assert_eq!(checkout.registered_worktrees(), 2);
    }

    #[tokio::test]
    async fn open_of_a_branch_past_open_is_refused() {
        let pool = crate::testdb::fresh_pool().await;
        let checkout = Checkout::new();
        seed_on(&pool, &checkout, vec![code(BRANCH, "building")]).await;

        let error = open_branch(&pool, TREE, BRANCH, &checkout.config())
            .await
            .unwrap_err();

        match error {
            OpenError::NotQueued { branch, state } => {
                assert_eq!(branch, BRANCH);
                assert_eq!(state, "building");
            }
            other => panic!("expected NotQueued, got {other:?}"),
        }
        checkout.assert_nothing_made();
        assert_eq!(branch_of(&pool, BRANCH).await.state, "building");
    }

    #[tokio::test]
    async fn open_refuses_an_existing_worktree_path_and_stays_queued() {
        let pool = crate::testdb::fresh_pool().await;
        let checkout = Checkout::new();
        seed_on(&pool, &checkout, vec![code(BRANCH, "queued")]).await;
        let taken = worktree_path(&checkout.repo, TREE, BRANCH);
        std::fs::create_dir_all(&taken).unwrap();

        let error = open_branch(&pool, TREE, BRANCH, &checkout.config())
            .await
            .unwrap_err();

        match error {
            OpenError::WorktreeExists(path) => assert_eq!(fwd(path), fwd(&taken)),
            other => panic!("expected WorktreeExists, got {other:?}"),
        }
        assert_eq!(checkout.registered_worktrees(), 1);
        assert_eq!(checkout.tree_refs(), "");
        assert_still_queued(&pool, BRANCH).await;
    }

    #[tokio::test]
    async fn a_git_failure_leaves_the_branch_queued() {
        let pool = crate::testdb::fresh_pool().await;
        let checkout = Checkout::new();
        seed_on(&pool, &checkout, vec![code(BRANCH, "queued")]).await;
        let config = OpenConfig {
            base_branch: "no-such-base".to_owned(),
            ..checkout.config()
        };

        let error = open_branch(&pool, TREE, BRANCH, &config).await.unwrap_err();

        assert!(matches!(error, OpenError::Git(_)), "{error:?}");
        checkout.assert_nothing_made();
        assert_still_queued(&pool, BRANCH).await;
    }

    #[tokio::test]
    async fn open_links_what_exists_and_lists_what_is_missing() {
        let pool = crate::testdb::fresh_pool().await;
        let checkout = Checkout::new();
        let source = checkout.repo.join("assets-untracked");
        std::fs::create_dir_all(&source).unwrap();
        std::fs::write(source.join("model.bin"), "weights\n").unwrap();
        seed_on(&pool, &checkout, vec![code(BRANCH, "queued")]).await;
        let config = OpenConfig {
            links: vec!["assets-untracked".to_owned(), "missing".to_owned()],
            ..checkout.config()
        };

        let opened = open_branch(&pool, TREE, BRANCH, &config).await.unwrap();

        assert_eq!(opened.skipped_links, ["missing"]);
        let through_the_link = PathBuf::from(opened.worktree.unwrap())
            .join("assets-untracked")
            .join("model.bin");
        assert_eq!(
            std::fs::read_to_string(through_the_link).unwrap(),
            "weights\n"
        );
    }

    /// A tracked directory is already in the new worktree, so the link cannot be made. The rollback
    /// must not reach through to the main checkout.
    #[tokio::test]
    async fn a_failing_link_rolls_back_worktree_and_branch_and_spares_the_source() {
        let pool = crate::testdb::fresh_pool().await;
        let checkout = Checkout::new();
        commit_file(&checkout.repo, "tracked/keep.txt", "keep\n");
        seed_on(&pool, &checkout, vec![code(BRANCH, "queued")]).await;
        let config = OpenConfig {
            links: vec!["tracked".to_owned()],
            ..checkout.config()
        };

        let error = open_branch(&pool, TREE, BRANCH, &config).await.unwrap_err();

        assert!(matches!(error, OpenError::Link(_)), "{error:?}");
        checkout.assert_nothing_made();
        assert_eq!(
            std::fs::read_to_string(checkout.repo.join("tracked/keep.txt")).unwrap(),
            "keep\n"
        );
        assert_still_queued(&pool, BRANCH).await;
    }

    /// `fresh_shared_pool`: on a single connection the two calls could not interleave at all.
    #[tokio::test]
    async fn two_opens_at_once_make_one_worktree() {
        let pool = crate::testdb::fresh_shared_pool().await;
        let checkout = Checkout::new();
        seed_on(&pool, &checkout, vec![code(BRANCH, "queued")]).await;
        let config = checkout.config();

        let (first, second) = tokio::join!(
            open_branch(&pool, TREE, BRANCH, &config),
            open_branch(&pool, TREE, BRANCH, &config),
        );

        let (first, second) = (first.unwrap(), second.unwrap());
        assert_ne!(first.replayed, second.replayed, "{first:?} {second:?}");
        assert_eq!(first.worktree, second.worktree);
        assert_eq!(checkout.registered_worktrees(), 2);
        pool.close().await;
    }

    /// The reason this module exists: without a worktree and a target dir, `commands` parks a leaf's
    /// every cargo command.
    #[tokio::test]
    async fn an_opened_branch_runs_its_cargo_command_instead_of_parking() {
        let pool = crate::testdb::fresh_pool().await;
        let checkout = Checkout::new();
        let paths = ["core/**".to_owned()];
        let mut opened_branch = code(BRANCH, "queued");
        opened_branch.paths = paths.to_vec();
        seed_on(&pool, &checkout, vec![opened_branch]).await;
        let opened = open_branch(&pool, TREE, BRANCH, &checkout.config())
            .await
            .unwrap();
        let cargo = ["cargo test -p nucleos-core some_test".to_owned()];
        let finished = || CommandRun {
            command: cargo[0].clone(),
            exit_code: Some(0),
            output: String::new(),
        };

        let seen = Mutex::new(Vec::new());
        let branch = branch_of(&pool, BRANCH).await;
        let outcome = commands::run_for_leaf(
            &cargo,
            &branch,
            Path::new("scripts/heavy.py"),
            |invocation: Invocation| {
                seen.lock().unwrap().push(invocation);
                let run = finished();
                async move { run }
            },
        )
        .await;

        assert!(matches!(outcome, RunOutcome::Ran(_)), "{outcome:?}");
        let seen = seen.into_inner().unwrap();
        assert_eq!(seen.len(), 1);
        assert_eq!(fwd(&seen[0].cwd), fwd(opened.worktree.as_deref().unwrap()));
        let target = seen[0]
            .env
            .iter()
            .find(|(name, _)| name == "CARGO_TARGET_DIR")
            .map(|(_, value)| value.clone());
        assert_eq!(target, opened.target_dir);

        let never_opened = Branch::new(OTHER_BRANCH, &paths, "core", None);
        let seen = Mutex::new(0);
        let outcome = commands::run_for_leaf(
            &cargo,
            &never_opened,
            Path::new("scripts/heavy.py"),
            |_: Invocation| {
                *seen.lock().unwrap() += 1;
                let run = finished();
                async move { run }
            },
        )
        .await;

        assert!(matches!(outcome, RunOutcome::Parked(_)), "{outcome:?}");
        assert_eq!(seen.into_inner().unwrap(), 0);
    }

    // ----- bind_branch -----

    #[tokio::test]
    async fn bind_sets_the_node_and_building_and_links_the_node_to_the_tree() {
        let pool = crate::testdb::fresh_pool().await;
        seed_elsewhere(&pool, vec![code(BRANCH, "open")]).await;
        seed_unlinked_node(&pool, NODE).await;

        let bound = bind_branch(&pool, TREE, BRANCH, NODE).await.unwrap();

        assert_eq!(
            bound,
            Bound {
                branch: BRANCH.to_owned(),
                node_id: NODE.to_owned(),
                state: "building".to_owned(),
                replayed: false,
            }
        );
        let branch = branch_of(&pool, BRANCH).await;
        assert_eq!(branch.node_id.as_deref(), Some(NODE));
        assert_eq!(branch.state, "building");
        assert_eq!(node_tree_job(&pool, NODE).await.as_deref(), Some(TREE));
    }

    #[tokio::test]
    async fn bind_of_a_docs_branch_works_from_queued() {
        let pool = crate::testdb::fresh_pool().await;
        seed_elsewhere(&pool, vec![docs(BRANCH, "queued")]).await;
        seed_unlinked_node(&pool, NODE).await;

        let bound = bind_branch(&pool, TREE, BRANCH, NODE).await.unwrap();

        assert_eq!(bound.state, "building");
        assert_eq!(branch_of(&pool, BRANCH).await.state, "building");
    }

    #[tokio::test]
    async fn bind_of_a_queued_code_branch_is_refused() {
        let pool = crate::testdb::fresh_pool().await;
        seed_elsewhere(&pool, vec![code(BRANCH, "queued")]).await;
        seed_unlinked_node(&pool, NODE).await;

        let error = bind_branch(&pool, TREE, BRANCH, NODE).await.unwrap_err();

        assert!(matches!(error, OpenError::NotQueued { .. }), "{error:?}");
        let branch = branch_of(&pool, BRANCH).await;
        assert_eq!(branch.state, "queued");
        assert_eq!(branch.node_id, None);
        assert_eq!(node_tree_job(&pool, NODE).await, None);
    }

    #[tokio::test]
    async fn bind_to_another_node_when_bound_is_refused() {
        let pool = crate::testdb::fresh_pool().await;
        let bound_branch = with_node(code(BRANCH, "building"), NODE);
        seed_elsewhere(&pool, vec![bound_branch]).await;
        seed_linked_node(&pool, NODE, None).await;
        seed_unlinked_node(&pool, OTHER_NODE).await;

        let error = bind_branch(&pool, TREE, BRANCH, OTHER_NODE)
            .await
            .unwrap_err();

        match error {
            OpenError::AlreadyBound { branch, node_id } => {
                assert_eq!(branch, BRANCH);
                assert_eq!(node_id, NODE);
            }
            other => panic!("expected AlreadyBound, got {other:?}"),
        }
        assert_eq!(
            branch_of(&pool, BRANCH).await.node_id.as_deref(),
            Some(NODE)
        );
        assert_eq!(node_tree_job(&pool, OTHER_NODE).await, None);
    }

    #[tokio::test]
    async fn a_second_bind_to_the_same_node_answers_replayed() {
        let pool = crate::testdb::fresh_pool().await;
        seed_elsewhere(&pool, vec![code(BRANCH, "open")]).await;
        seed_unlinked_node(&pool, NODE).await;
        bind_branch(&pool, TREE, BRANCH, NODE).await.unwrap();

        let second = bind_branch(&pool, TREE, BRANCH, NODE).await.unwrap();

        assert!(second.replayed);
        assert_eq!(second.state, "building");

        // Still a replay once the branch has moved on, and it does not move the branch back.
        let mut job = store::load(&pool, TREE).await.unwrap().unwrap();
        job.manifest.branches.get_mut(BRANCH).unwrap().state = "gating".to_owned();
        store::update(&pool, &job).await.unwrap();

        let third = bind_branch(&pool, TREE, BRANCH, NODE).await.unwrap();

        assert!(third.replayed);
        assert_eq!(third.state, "gating");
        assert_eq!(branch_of(&pool, BRANCH).await.state, "gating");
    }

    #[tokio::test]
    async fn bind_of_a_node_of_another_branch_is_refused() {
        let pool = crate::testdb::fresh_pool().await;
        let branches = vec![
            code(BRANCH, "open"),
            with_node(code(OTHER_BRANCH, "building"), NODE),
        ];
        seed_elsewhere(&pool, branches).await;
        seed_linked_node(&pool, NODE, None).await;

        let error = bind_branch(&pool, TREE, BRANCH, NODE).await.unwrap_err();

        match error {
            OpenError::NodeTaken { node_id, branch } => {
                assert_eq!(node_id, NODE);
                assert_eq!(branch, OTHER_BRANCH);
            }
            other => panic!("expected NodeTaken, got {other:?}"),
        }
        let branch = branch_of(&pool, BRANCH).await;
        assert_eq!(branch.state, "open");
        assert_eq!(branch.node_id, None);
    }

    #[tokio::test]
    async fn bind_of_an_unknown_node_is_not_found() {
        let pool = crate::testdb::fresh_pool().await;
        seed_elsewhere(&pool, vec![code(BRANCH, "open")]).await;

        let error = bind_branch(&pool, TREE, BRANCH, "no-such-node")
            .await
            .unwrap_err();

        assert!(matches!(error, OpenError::NodeNotFound(_)), "{error:?}");
        let branch = branch_of(&pool, BRANCH).await;
        assert_eq!(branch.state, "open");
        assert_eq!(branch.node_id, None);
    }

    #[tokio::test]
    async fn bind_of_a_node_of_another_tree_is_refused() {
        let pool = crate::testdb::fresh_pool().await;
        seed_elsewhere(&pool, vec![code(BRANCH, "open")]).await;
        let other = job_of(OTHER_TREE, NOWHERE, FEATURE, Vec::new());
        store::save(&pool, &other).await.unwrap();
        // Under the other tree's root, and not linked to it yet.
        seed_node_row(&pool, "by-root", OTHER_ROOT, None, None).await;
        // Under this root, yet already linked to the other tree.
        seed_node_row(&pool, "by-link", ROOT, Some(OTHER_TREE), None).await;

        for node in ["by-root", "by-link"] {
            let error = bind_branch(&pool, TREE, BRANCH, node).await.unwrap_err();

            match error {
                OpenError::ForeignNode { node_id } => assert_eq!(node_id, node),
                other => panic!("{node}: expected ForeignNode, got {other:?}"),
            }
        }
        let branch = branch_of(&pool, BRANCH).await;
        assert_eq!(branch.state, "open");
        assert_eq!(branch.node_id, None);
        assert_eq!(node_tree_job(&pool, "by-root").await, None);
    }

    /// The whole path a controller takes, against the route that refused every node opened through
    /// `/nodes` for having no `tree_job_id`.
    #[tokio::test]
    async fn a_bound_node_reaches_the_transition_route() {
        let state = live_agent_state().await;
        let app = both_apps(&state);
        let checkout = Checkout::with_floor("0");
        seed_for_route(&state.pool, &checkout, vec![code(BRANCH, "queued")]).await;
        seed_unlinked_node(&state.pool, NODE).await;

        let opened = send(&app, &open_uri(TREE, BRANCH), Body::empty()).await;
        assert_eq!(opened.0, StatusCode::OK, "{:?}", opened.1);
        let bound = post(&app, &bind_uri(TREE, BRANCH), json!({ "node_id": NODE })).await;
        assert_eq!(bound.0, StatusCode::OK, "{:?}", bound.1);

        let moved = post(
            &app,
            &transition_uri(TREE, NODE),
            json!({ "transition": "gating", "key": KEY }),
        )
        .await;

        assert_eq!(moved.0, StatusCode::OK, "{:?}", moved.1);
        assert_eq!(branch_of(&state.pool, BRANCH).await.state, "gating");
    }

    // ----- routes -----

    #[tokio::test]
    async fn post_open_answers_paths_env_and_sync() {
        let state = live_agent_state().await;
        let checkout = Checkout::with_floor("0");
        seed_for_route(&state.pool, &checkout, vec![code(BRANCH, "queued")]).await;

        let (status, body) = post(&app(&state), &open_uri(TREE, BRANCH), json!({})).await;

        assert_eq!(status, StatusCode::OK, "{body:?}");
        let worktree = body["worktree"].as_str().expect("worktree");
        assert_eq!(
            fwd(worktree),
            fwd(worktree_path(&checkout.repo, TREE, BRANCH))
        );
        assert!(Path::new(worktree).is_dir());
        assert_eq!(body["git_branch"], "tree/job-1/api");
        let target = body["target_dir"].as_str().expect("target_dir");
        assert!(
            fwd(target).ends_with("cargo-target/job-1-api"),
            "{target:?}"
        );
        assert_eq!(body["env"]["CARGO_TARGET_DIR"], target);
        assert_eq!(body["skipped_links"], json!([]));
        assert!(body["workflow_sync"].is_string(), "{body:?}");
        assert_eq!(body["replayed"], false);
    }

    #[tokio::test]
    async fn post_open_with_no_body_or_an_empty_object_works() {
        let state = live_agent_state().await;
        let app = app(&state);
        let checkout = Checkout::with_floor("0");
        let branches = vec![code(BRANCH, "queued"), code(OTHER_BRANCH, "queued")];
        seed_for_route(&state.pool, &checkout, branches).await;

        let bodiless = send(&app, &open_uri(TREE, BRANCH), Body::empty()).await;
        let empty = post(&app, &open_uri(TREE, OTHER_BRANCH), json!({})).await;

        assert_eq!(bodiless.0, StatusCode::OK, "{:?}", bodiless.1);
        assert_eq!(empty.0, StatusCode::OK, "{:?}", empty.1);
        assert_eq!(checkout.registered_worktrees(), 3);
    }

    #[tokio::test]
    async fn post_open_of_unknown_job_or_branch_is_404() {
        let state = live_agent_state().await;
        let app = app(&state);
        let checkout = Checkout::with_floor("0");
        seed_for_route(&state.pool, &checkout, vec![code(BRANCH, "queued")]).await;

        let unknown_job = post(&app, &open_uri("no-such-job", BRANCH), json!({})).await;
        let unknown_branch = post(&app, &open_uri(TREE, "no-such-branch"), json!({})).await;

        assert_error(&unknown_job, StatusCode::NOT_FOUND);
        assert_error(&unknown_branch, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn post_open_not_admitted_is_409_with_reasons() {
        let state = live_agent_state().await;
        let checkout = Checkout::with_floor("1000000000");
        seed_for_route(&state.pool, &checkout, vec![code(BRANCH, "queued")]).await;

        let response = post(&app(&state), &open_uri(TREE, BRANCH), json!({})).await;

        assert_error(&response, StatusCode::CONFLICT);
        let reasons = response.1["reasons"].as_array().expect("reasons");
        assert!(!reasons.is_empty(), "{:?}", response.1);
        checkout.assert_nothing_made();
        assert_still_queued(&state.pool, BRANCH).await;
    }

    #[tokio::test]
    async fn post_open_of_a_bad_name_is_422() {
        let state = live_agent_state().await;
        let checkout = Checkout::with_floor("0");
        let branches = vec![code(".hidden", "queued"), code("a b", "queued")];
        seed_for_route(&state.pool, &checkout, branches).await;

        for name in [".hidden", "a%20b"] {
            let response = post(&app(&state), &open_uri(TREE, name), json!({})).await;

            assert_error(&response, StatusCode::UNPROCESSABLE_ENTITY);
        }
        checkout.assert_nothing_made();
    }

    #[tokio::test]
    async fn post_bind_answers_node_and_state() {
        let state = live_agent_state().await;
        seed_elsewhere(&state.pool, vec![code(BRANCH, "open")]).await;
        seed_unlinked_node(&state.pool, NODE).await;

        let (status, body) = post(
            &app(&state),
            &bind_uri(TREE, BRANCH),
            json!({ "node_id": NODE }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body:?}");
        assert_eq!(
            body,
            json!({ "name": BRANCH, "node_id": NODE, "state": "building", "replayed": false })
        );
    }

    #[tokio::test]
    async fn post_bind_missing_or_null_node_id_is_400() {
        let state = live_agent_state().await;
        let app = app(&state);
        seed_elsewhere(&state.pool, vec![code(BRANCH, "open")]).await;

        for body in [json!({}), json!({ "node_id": null })] {
            let response = post(&app, &bind_uri(TREE, BRANCH), body).await;

            assert_error(&response, StatusCode::BAD_REQUEST);
        }
        assert_eq!(branch_of(&state.pool, BRANCH).await.state, "open");
    }

    #[tokio::test]
    async fn post_bind_bad_json_is_400() {
        let state = live_agent_state().await;
        seed_elsewhere(&state.pool, vec![code(BRANCH, "open")]).await;

        let response = send(
            &app(&state),
            &bind_uri(TREE, BRANCH),
            Body::from("{not json"),
        )
        .await;

        assert_error(&response, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn post_bind_unknown_node_is_404() {
        let state = live_agent_state().await;
        seed_elsewhere(&state.pool, vec![code(BRANCH, "open")]).await;

        let response = post(
            &app(&state),
            &bind_uri(TREE, BRANCH),
            json!({ "node_id": "no-such-node" }),
        )
        .await;

        assert_error(&response, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn post_bind_second_node_is_409() {
        let state = live_agent_state().await;
        let app = app(&state);
        seed_elsewhere(&state.pool, vec![code(BRANCH, "open")]).await;
        seed_unlinked_node(&state.pool, NODE).await;
        seed_unlinked_node(&state.pool, OTHER_NODE).await;
        let first = post(&app, &bind_uri(TREE, BRANCH), json!({ "node_id": NODE })).await;
        assert_eq!(first.0, StatusCode::OK, "{:?}", first.1);

        let second = post(
            &app,
            &bind_uri(TREE, BRANCH),
            json!({ "node_id": OTHER_NODE }),
        )
        .await;

        assert_error(&second, StatusCode::CONFLICT);
    }

    // ----- go containment -----

    fn go_validate(cmd: &str, glob: &str) -> Result<commands::Allowed, commands::Parked> {
        commands::validate(cmd, &[glob.to_owned()])
    }

    /// The command is allowed and its argv is exactly what was written.
    fn assert_go_runs(cmd: &str, glob: &str) {
        match go_validate(cmd, glob) {
            Ok(allowed) => assert_eq!(
                allowed.argv,
                cmd.split(' ').map(str::to_owned).collect::<Vec<_>>()
            ),
            Err(parked) => panic!("{cmd:?} must run under {glob:?}: {}", parked.needs),
        }
    }

    fn assert_go_parks(cmd: &str, glob: &str) {
        assert!(
            go_validate(cmd, glob).is_err(),
            "{cmd:?} must park under {glob:?}"
        );
    }

    #[test]
    fn go_dash_c_of_a_module_runs_under_a_narrower_glob_inside_it() {
        for cmd in [
            "go test -C sidecars/email ./...",
            "go test -C sidecars/email -run TestSend ./...",
            "go test -C=sidecars/email ./...",
        ] {
            assert_go_runs(cmd, "sidecars/email/internal/**");
        }
    }

    #[test]
    fn go_dash_c_with_a_named_package_held_by_the_glob_runs() {
        assert_go_runs(
            "go test -C sidecars/email ./internal/...",
            "sidecars/email/internal/**",
        );
    }

    #[test]
    fn go_dash_c_with_a_named_package_outside_the_glob_parks() {
        assert_go_parks(
            "go test -C sidecars/email ./cmd/...",
            "sidecars/email/internal/**",
        );
    }

    #[test]
    fn go_dash_c_of_a_parent_of_the_module_parks() {
        assert_go_parks("go test -C sidecars ./...", "sidecars/email/**");
    }

    #[test]
    fn go_dash_c_of_a_directory_inside_a_module_parks_under_a_narrower_glob() {
        assert_go_parks(
            "go test -C sidecars/email/internal ./...",
            "sidecars/email/internal/foo/**",
        );
    }

    #[test]
    fn go_dash_c_of_another_module_parks() {
        for cmd in [
            "go test -C sidecars/telegram ./...",
            "go test -C sidecars/emailx ./...",
        ] {
            assert_go_parks(cmd, "sidecars/email/internal/**");
        }
    }

    #[test]
    fn go_without_dash_c_stays_parked_under_a_narrower_glob() {
        assert_go_parks("go test ./sidecars/email/...", "sidecars/email/internal/**");
    }

    #[test]
    fn a_glob_outside_the_go_module_does_not_hold_it() {
        assert_go_parks("go test -C core ./...", "core/src/**");
    }

    // ----- corrupt manifest -----

    async fn store_dated(pool: &SqlitePool, id: &str, created_at: &str) {
        let mut job = job_of(id, NOWHERE, FEATURE, Vec::new());
        job.created_at = created_at.to_owned();
        store::save(pool, &job).await.unwrap();
    }

    async fn corrupt(pool: &SqlitePool, id: &str) {
        sqlx::query("UPDATE tree_jobs SET manifest = '{not json' WHERE id = ?")
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// A job holding a running node on a run that is gone, the case `reconcile_running` acts on.
    async fn seed_job_with_dead_node(pool: &SqlitePool, id: &str, node: &str, run_id: i64) {
        store_dated(pool, id, CREATED).await;
        seed_run(pool, run_id, "interrupted").await;
        seed_node_row(pool, node, &format!("{id}-root"), Some(id), Some(run_id)).await;
    }

    #[tokio::test]
    async fn list_non_terminal_skips_a_corrupt_job_and_keeps_the_others() {
        let pool = crate::testdb::fresh_pool().await;
        for (id, at) in [
            ("good-1", "2026-10-30T10:00:00+00:00"),
            ("bad-1", "2026-10-30T10:00:01+00:00"),
            ("good-2", "2026-10-30T10:00:02+00:00"),
            ("bad-2", "2026-10-30T10:00:03+00:00"),
            ("good-3", "2026-10-30T10:00:04+00:00"),
        ] {
            store_dated(&pool, id, at).await;
        }
        corrupt(&pool, "bad-1").await;
        corrupt(&pool, "bad-2").await;

        let listed: Vec<String> = store::list_non_terminal(&pool)
            .await
            .unwrap()
            .into_iter()
            .map(|job| job.id)
            .collect();

        assert_eq!(listed, ["good-1", "good-2", "good-3"]);
    }

    #[tokio::test]
    async fn list_non_terminal_writes_a_feed_line_naming_the_corrupt_job() {
        let pool = crate::testdb::fresh_pool().await;
        store_dated(&pool, "bad-1", CREATED).await;
        corrupt(&pool, "bad-1").await;

        store::list_non_terminal(&pool).await.unwrap();

        let lines = feed_summaries(&pool, CORRUPT_KIND).await;
        assert!(lines.iter().any(|line| line.contains("bad-1")), "{lines:?}");
    }

    /// One bad row must not stop a list, but a read of that very job must still say what is wrong.
    #[tokio::test]
    async fn load_of_a_corrupt_job_still_answers_the_manifest_error() {
        let pool = crate::testdb::fresh_pool().await;
        store_dated(&pool, "bad-1", CREATED).await;
        corrupt(&pool, "bad-1").await;

        let error = store::load(&pool, "bad-1").await.unwrap_err();

        assert!(matches!(error, StoreError::Manifest(_)), "{error:?}");
    }

    #[tokio::test]
    async fn reconcile_running_skips_a_corrupt_job_and_fails_the_others() {
        let pool = crate::testdb::fresh_pool().await;
        seed_job_with_dead_node(&pool, "bad-1", "node-bad", 4801).await;
        seed_job_with_dead_node(&pool, "good-1", "node-good", 4802).await;
        corrupt(&pool, "bad-1").await;

        let marked = node_run::reconcile_running(&pool).await.unwrap();

        assert_eq!(marked, ["node-good"]);
        assert_eq!(node_status(&pool, "node-good").await, "failed");
        assert_eq!(node_status(&pool, "node-bad").await, "running");
        let lines = feed_summaries(&pool, CORRUPT_KIND).await;
        assert!(lines.iter().any(|line| line.contains("bad-1")), "{lines:?}");
    }

    #[tokio::test]
    async fn resume_all_survives_a_corrupt_job() {
        let pool = crate::testdb::fresh_pool().await;
        seed_job_with_dead_node(&pool, "bad-1", "node-bad", 4801).await;
        seed_job_with_dead_node(&pool, "good-1", "node-good", 4802).await;
        corrupt(&pool, "bad-1").await;

        let resumed = store::resume_all(&pool).await.unwrap();

        assert_eq!(resumed.jobs, ["good-1"]);
        assert_eq!(resumed.failed_nodes, ["node-good"]);
        assert_eq!(node_status(&pool, "node-bad").await, "running");
    }

    // ----- launch prompt -----

    /// One open job `TREE` holding the running node `NODE`, which has no run yet.
    async fn state_with_node() -> AppState {
        let state = live_agent_state().await;
        seed_elsewhere(&state.pool, Vec::new()).await;
        seed_linked_node(&state.pool, NODE, None).await;
        state
    }

    #[tokio::test]
    async fn save_launch_keeps_the_first_write_per_key() {
        let pool = crate::testdb::fresh_pool().await;
        seed_elsewhere(&pool, Vec::new()).await;
        seed_linked_node(&pool, NODE, None).await;
        let first = launch_of(NODE, KEY, "shadow");
        let second = StoredLaunch {
            prompt: "another prompt".to_owned(),
            mode: "real".to_owned(),
            ..first.clone()
        };

        save_launch(&pool, &first).await.unwrap();
        save_launch(&pool, &second).await.unwrap();

        assert_eq!(stored_launch(&pool, NODE, KEY).await.unwrap(), Some(first));
        assert_eq!(stored_launch(&pool, NODE, OTHER_KEY).await.unwrap(), None);
    }

    #[tokio::test]
    async fn latest_launch_answers_the_last_saved() {
        let pool = crate::testdb::fresh_pool().await;
        seed_elsewhere(&pool, Vec::new()).await;
        seed_linked_node(&pool, NODE, None).await;
        assert_eq!(latest_launch(&pool, NODE).await.unwrap(), None);
        // Written in an order neither the keys nor a clock could reconstruct.
        let older = launch_of(NODE, "z-older", "shadow");
        let newer = StoredLaunch {
            prompt: "the newer prompt".to_owned(),
            ..launch_of(NODE, "a-newer", "shadow")
        };

        save_launch(&pool, &older).await.unwrap();
        save_launch(&pool, &newer).await.unwrap();

        assert_eq!(latest_launch(&pool, NODE).await.unwrap(), Some(newer));
    }

    /// Stored before the create, or a crash between the two would leave nothing to relaunch: a
    /// create that fails still leaves the launch behind.
    #[tokio::test]
    async fn launch_recovering_stores_the_launch_before_starting() {
        let state = state_with_node().await;
        let cwd = fwd(std::env::temp_dir());

        store::launch_recovering(
            &state,
            NODE,
            KEY,
            PROMPT.to_owned(),
            Some(PROJECT.to_owned()),
            Some(cwd.clone()),
            "shadow",
        )
        .await
        .unwrap();

        assert_eq!(
            stored_launch(&state.pool, NODE, KEY).await.unwrap(),
            Some(StoredLaunch {
                node_id: NODE.to_owned(),
                key: KEY.to_owned(),
                prompt: PROMPT.to_owned(),
                project_id: Some(PROJECT.to_owned()),
                cwd: Some(cwd),
                mode: "shadow".to_owned(),
            })
        );

        let failed = store::launch_recovering(
            &state,
            NODE,
            OTHER_KEY,
            PROMPT.to_owned(),
            None,
            None,
            REFUSED_WITHOUT_A_PROJECT,
        )
        .await;

        assert!(failed.is_err(), "{failed:?}");
        assert_eq!(
            stored_launch(&state.pool, NODE, OTHER_KEY).await.unwrap(),
            Some(launch_of(NODE, OTHER_KEY, REFUSED_WITHOUT_A_PROJECT))
        );
    }

    #[tokio::test]
    async fn the_launch_route_stores_the_prompt() {
        let state = state_with_node().await;

        let (status, body) = post(
            &tree_app(&state),
            &transition_uri(TREE, NODE),
            json!({ "transition": "launch", "key": KEY, "prompt": PROMPT, "mode": "shadow" }),
        )
        .await;

        assert_eq!(status, StatusCode::OK, "{body:?}");
        let stored = stored_launch(&state.pool, NODE, KEY)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.prompt, PROMPT);
        assert_eq!(stored.mode, "shadow");
        assert_eq!(stored.project_id.as_deref(), Some(PROJECT));
    }

    #[tokio::test]
    async fn a_replayed_launch_keeps_the_first_prompt() {
        let state = state_with_node().await;
        let launch = |prompt: &str| {
            store::launch_recovering(&state, NODE, KEY, prompt.to_owned(), None, None, "shadow")
        };

        let first = launch(PROMPT).await.unwrap();
        let replay = launch("another prompt").await.unwrap();

        assert_eq!(replay, first);
        assert_eq!(run_rows(&state.pool).await, 1);
        let stored = stored_launch(&state.pool, NODE, KEY)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.prompt, PROMPT);
    }

    // ----- relaunch_resumed -----

    /// `NODE` running on a run that is gone, on top of a branch in `state`; the node's launch is
    /// stored under `KEY`.
    async fn seed_dead_branch_node(state: &AppState, branch_state: &str) {
        let pool = &state.pool;
        let branch = with_node(code(BRANCH, branch_state), NODE);
        seed_elsewhere(pool, vec![branch]).await;
        seed_run(pool, DEAD_RUN, "interrupted").await;
        seed_linked_node(pool, NODE, Some(DEAD_RUN)).await;
        save_shadow_launch(pool, NODE, KEY).await;
    }

    #[tokio::test]
    async fn a_cleared_launch_starts_one_run_with_the_stored_prompt_and_key() {
        let state = state_with_node().await;
        let pool = &state.pool;
        save_shadow_launch(pool, NODE, KEY).await;
        reserve(pool, NODE, KEY).await;

        let resumed = store::resume_all(pool).await.unwrap();
        assert_eq!(
            resumed.cleared_launches,
            [(NODE.to_owned(), KEY.to_owned())]
        );

        let relaunched = relaunch_resumed(&state, &resumed).await;

        assert_eq!(relaunched.started.len(), 1, "{relaunched:?}");
        let (node, run_id) = relaunched.started[0].clone();
        assert_eq!(node, NODE);
        assert_eq!(run_rows(pool).await, 1);
        assert_eq!(
            stored_result(pool, NODE, "launch", KEY).await,
            Some(run_id.to_string())
        );
        assert_eq!(run_prompt(pool, run_id).await, PROMPT);
    }

    #[tokio::test]
    async fn a_cleared_launch_with_no_stored_prompt_is_skipped_with_a_feed_line() {
        let state = state_with_node().await;
        let pool = &state.pool;
        reserve(pool, NODE, KEY).await;
        let resumed = store::resume_all(pool).await.unwrap();

        let relaunched = relaunch_resumed(&state, &resumed).await;

        assert_started_nothing(pool, &relaunched, 0).await;
        assert!(
            relaunched.skipped.iter().any(|(node, _)| node == NODE),
            "{relaunched:?}"
        );
        assert!(!feed_summaries(pool, RELAUNCH_SKIPPED_KIND).await.is_empty());
    }

    #[tokio::test]
    async fn a_spared_branch_node_whose_run_died_relaunches_from_its_latest_launch() {
        let state = live_agent_state().await;
        let pool = &state.pool;
        seed_dead_branch_node(&state, "building").await;
        let resumed = store::resume_all(pool).await.unwrap();
        assert_eq!(resumed.failed_nodes, Vec::<String>::new());
        assert_eq!(node_status(pool, NODE).await, "running");

        let relaunched = relaunch_resumed(&state, &resumed).await;

        assert_eq!(relaunched.started.len(), 1, "{relaunched:?}");
        let (node, run_id) = relaunched.started[0].clone();
        assert_eq!(node, NODE);
        assert_ne!(run_id, DEAD_RUN);
        assert_eq!(node_run_id(pool, NODE).await, Some(run_id.to_string()));
        assert_eq!(run_prompt(pool, run_id).await, PROMPT);
    }

    #[tokio::test]
    async fn relaunch_resumed_still_uses_the_resume_key() {
        let state = live_agent_state().await;
        let pool = &state.pool;
        seed_dead_branch_node(&state, "building").await;
        let resumed = store::resume_all(pool).await.unwrap();

        let relaunched = relaunch_resumed(&state, &resumed).await;

        let (_, run_id) = relaunched.started[0];
        assert_eq!(
            stored_result(pool, NODE, "launch", &format!("resume-{DEAD_RUN}")).await,
            Some(run_id.to_string())
        );
    }

    #[tokio::test]
    async fn a_failed_node_is_not_relaunched() {
        let state = live_agent_state().await;
        let pool = &state.pool;
        seed_elsewhere(pool, Vec::new()).await;
        seed_run(pool, DEAD_RUN, "interrupted").await;
        seed_linked_node(pool, NODE, Some(DEAD_RUN)).await;
        save_shadow_launch(pool, NODE, KEY).await;
        let resumed = store::resume_all(pool).await.unwrap();
        assert_eq!(resumed.failed_nodes, [NODE]);

        let relaunched = relaunch_resumed(&state, &resumed).await;

        assert_started_nothing(pool, &relaunched, 1).await;
        assert_eq!(node_status(pool, NODE).await, "failed");
    }

    /// A parked branch waits for its owner's answer, and only the answer starts it again.
    #[tokio::test]
    async fn a_parked_branch_node_is_not_relaunched_at_boot() {
        // With a key cleared by the boot.
        let state = live_agent_state().await;
        let pool = &state.pool;
        let branch = with_node(code(BRANCH, "parked"), NODE);
        seed_elsewhere(pool, vec![branch]).await;
        seed_linked_node(pool, NODE, None).await;
        save_shadow_launch(pool, NODE, KEY).await;
        reserve(pool, NODE, KEY).await;
        let resumed = store::resume_all(pool).await.unwrap();
        assert_eq!(resumed.cleared_launches.len(), 1);

        let relaunched = relaunch_resumed(&state, &resumed).await;

        assert_started_nothing(pool, &relaunched, 0).await;

        // With a run that died.
        let state = live_agent_state().await;
        seed_dead_branch_node(&state, "parked").await;
        let resumed = store::resume_all(&state.pool).await.unwrap();

        let relaunched = relaunch_resumed(&state, &resumed).await;

        assert_started_nothing(&state.pool, &relaunched, 1).await;
        assert_eq!(
            node_run_id(&state.pool, NODE).await,
            Some(DEAD_RUN.to_string())
        );
    }

    #[tokio::test]
    async fn a_live_run_is_not_relaunched() {
        let state = live_agent_state().await;
        let pool = &state.pool;
        let branch = with_node(code(BRANCH, "building"), NODE);
        seed_elsewhere(pool, vec![branch]).await;
        seed_run(pool, LIVE_RUN, "running").await;
        seed_linked_node(pool, NODE, Some(LIVE_RUN)).await;
        save_shadow_launch(pool, NODE, KEY).await;
        let resumed = store::resume_all(pool).await.unwrap();

        let relaunched = relaunch_resumed(&state, &resumed).await;

        assert_started_nothing(pool, &relaunched, 1).await;
        assert_eq!(node_run_id(pool, NODE).await, Some(LIVE_RUN.to_string()));
    }

    #[tokio::test]
    async fn a_finished_job_is_not_relaunched() {
        let state = live_agent_state().await;
        let pool = &state.pool;
        seed_dead_branch_node(&state, "building").await;
        finish_job(pool, TREE).await;
        let resumed = store::resume_all(pool).await.unwrap();
        assert_eq!(resumed.jobs, Vec::<String>::new());

        let relaunched = relaunch_resumed(&state, &resumed).await;

        assert_started_nothing(pool, &relaunched, 1).await;
    }

    #[tokio::test]
    async fn one_failing_relaunch_does_not_stop_the_others() {
        let state = live_agent_state().await;
        let pool = &state.pool;
        seed_elsewhere(pool, Vec::new()).await;
        for (node, mode) in [
            ("a-fails", REFUSED_WITHOUT_A_PROJECT),
            ("b-starts", "shadow"),
        ] {
            seed_linked_node(pool, node, None).await;
            save_launch(pool, &launch_of(node, KEY, mode))
                .await
                .unwrap();
            reserve(pool, node, KEY).await;
        }
        let resumed = store::resume_all(pool).await.unwrap();

        let relaunched = relaunch_resumed(&state, &resumed).await;

        assert_eq!(relaunched.started.len(), 1, "{relaunched:?}");
        assert_eq!(relaunched.started[0].0, "b-starts");
        assert!(
            relaunched.skipped.iter().any(|(node, _)| node == "a-fails"),
            "{relaunched:?}"
        );
        assert_eq!(run_rows(pool).await, 1);
    }

    /// A pass is keyed on the dead run, so this holds only while the new run is live: a new run that
    /// had already ended would be a new dead run with a new key. `live_agent_state` keeps it alive
    /// for the length of the test.
    #[tokio::test]
    async fn a_second_relaunch_pass_starts_no_second_run() {
        let state = live_agent_state().await;
        let pool = &state.pool;
        // A branch node on a dead run, and a leaf with a cleared key.
        seed_dead_branch_node(&state, "building").await;
        seed_linked_node(pool, OTHER_NODE, None).await;
        save_shadow_launch(pool, OTHER_NODE, OTHER_KEY).await;
        reserve(pool, OTHER_NODE, OTHER_KEY).await;
        let resumed = store::resume_all(pool).await.unwrap();

        let first = relaunch_resumed(&state, &resumed).await;
        assert_eq!(first.started.len(), 2, "{first:?}");
        let rows = run_rows(pool).await;
        assert_eq!(rows, 3, "the dead run and two new ones");

        relaunch_resumed(&state, &resumed).await;

        assert_eq!(run_rows(pool).await, rows);
    }

    // ----- answer_and_relaunch -----

    /// `NODE` on a branch that is `building`, parked on a question; the run that asked has ended.
    /// Answers the id of the open decision. `launch_mode` is the mode of the stored launch, if any.
    async fn seed_parked_decision(
        state: &AppState,
        launch_mode: Option<&str>,
        run_status: &str,
    ) -> String {
        let pool = &state.pool;
        let branch = with_node(code(BRANCH, "building"), NODE);
        seed_elsewhere(pool, vec![branch]).await;
        seed_linked_node(pool, NODE, None).await;
        if let Some(mode) = launch_mode {
            save_launch(pool, &launch_of(NODE, KEY, mode))
                .await
                .unwrap();
        }
        let parked = decisions::park(pool, TREE, NODE, QUESTION).await.unwrap();
        seed_run(pool, DEAD_RUN, run_status).await;
        sqlx::query("UPDATE work_nodes SET run_id = ? WHERE id = ?")
            .bind(DEAD_RUN.to_string())
            .bind(NODE)
            .execute(pool)
            .await
            .unwrap();
        parked.id
    }

    #[tokio::test]
    async fn an_answer_relaunches_the_node_of_the_resumed_branch() {
        let state = live_agent_state().await;
        let pool = &state.pool;
        let decision = seed_parked_decision(&state, Some("shadow"), "succeeded").await;
        assert_eq!(branch_of(pool, BRANCH).await.state, "parked");

        let answered = answer_and_relaunch(&state, &decision, ANSWER, CHANNEL)
            .await
            .unwrap();

        let run_id = answered.relaunched.expect("a relaunch");
        assert_eq!(node_run_id(pool, NODE).await, Some(run_id.to_string()));
        assert_eq!(branch_of(pool, BRANCH).await.state, "building");
        assert_eq!(answered.decision.id, decision);
        assert_eq!(answered.decision.status, "answered");
        assert_eq!(answered.decision.answer.as_deref(), Some(ANSWER));
        assert_eq!(answered.decision.origin.as_deref(), Some(CHANNEL));
    }

    #[tokio::test]
    async fn the_relaunch_prompt_holds_the_stored_prompt_and_the_answer() {
        let state = live_agent_state().await;
        let decision = seed_parked_decision(&state, Some("shadow"), "succeeded").await;

        let answered = answer_and_relaunch(&state, &decision, ANSWER, CHANNEL)
            .await
            .unwrap();

        let prompt = run_prompt(&state.pool, answered.relaunched.unwrap()).await;
        assert!(prompt.contains(PROMPT), "{prompt:?}");
        assert!(prompt.contains(ANSWER), "{prompt:?}");
    }

    #[tokio::test]
    async fn the_key_of_an_answer_relaunch_is_derived_from_the_decision() {
        let state = live_agent_state().await;
        let decision = seed_parked_decision(&state, Some("shadow"), "succeeded").await;

        let answered = answer_and_relaunch(&state, &decision, ANSWER, CHANNEL)
            .await
            .unwrap();

        let key = format!("decision-{decision}");
        assert_eq!(
            stored_result(&state.pool, NODE, "launch", &key).await,
            Some(answered.relaunched.unwrap().to_string())
        );
    }

    #[tokio::test]
    async fn an_answer_with_a_live_run_starts_nothing() {
        let state = live_agent_state().await;
        let pool = &state.pool;
        let decision = seed_parked_decision(&state, Some("shadow"), "running").await;
        let rows = run_rows(pool).await;

        let answered = answer_and_relaunch(&state, &decision, ANSWER, CHANNEL)
            .await
            .unwrap();

        assert_eq!(answered.relaunched, None);
        assert_eq!(answered.decision.status, "answered");
        assert_eq!(run_rows(pool).await, rows);
        assert_eq!(node_run_id(pool, NODE).await, Some(DEAD_RUN.to_string()));
    }

    #[tokio::test]
    async fn an_answer_with_no_stored_launch_keeps_the_answer_and_writes_a_feed_line() {
        let state = live_agent_state().await;
        let pool = &state.pool;
        let decision = seed_parked_decision(&state, None, "succeeded").await;
        let rows = run_rows(pool).await;

        let answered = answer_and_relaunch(&state, &decision, ANSWER, CHANNEL)
            .await
            .unwrap();

        assert_eq!(answered.relaunched, None);
        assert_eq!(answered.decision.status, "answered");
        assert_eq!(answered.decision.answer.as_deref(), Some(ANSWER));
        assert_eq!(branch_of(pool, BRANCH).await.state, "building");
        assert_eq!(run_rows(pool).await, rows);
        assert!(!feed_summaries(pool, RELAUNCH_SKIPPED_KIND).await.is_empty());
    }

    #[tokio::test]
    async fn a_failing_relaunch_keeps_the_answer() {
        let state = live_agent_state().await;
        let pool = &state.pool;
        let decision =
            seed_parked_decision(&state, Some(REFUSED_WITHOUT_A_PROJECT), "succeeded").await;
        let rows = run_rows(pool).await;

        let answered = answer_and_relaunch(&state, &decision, ANSWER, CHANNEL)
            .await
            .unwrap();

        assert_eq!(answered.relaunched, None);
        assert_eq!(answered.decision.status, "answered");
        assert_eq!(run_rows(pool).await, rows);
        assert!(!feed_summaries(pool, RELAUNCH_SKIPPED_KIND).await.is_empty());
        let key = format!("decision-{decision}");
        assert_eq!(
            stored_result(pool, NODE, "launch", &key).await,
            None,
            "the key must be free for another try"
        );
    }

    #[tokio::test]
    async fn an_answer_of_an_unknown_decision_is_an_error_and_starts_nothing() {
        let state = live_agent_state().await;
        let pool = &state.pool;
        seed_parked_decision(&state, Some("shadow"), "succeeded").await;
        let rows = run_rows(pool).await;

        let answered = answer_and_relaunch(&state, "no-such-decision", ANSWER, CHANNEL).await;

        assert!(answered.is_err(), "{answered:?}");
        assert_eq!(run_rows(pool).await, rows);
        assert_eq!(branch_of(pool, BRANCH).await.state, "parked");
    }

    #[tokio::test]
    async fn a_second_answer_starts_no_second_run() {
        let state = live_agent_state().await;
        let pool = &state.pool;
        let decision = seed_parked_decision(&state, Some("shadow"), "succeeded").await;
        let first = answer_and_relaunch(&state, &decision, ANSWER, CHANNEL)
            .await
            .unwrap();
        let run_id = first.relaunched.unwrap();
        let rows = run_rows(pool).await;

        let second = answer_and_relaunch(&state, &decision, "Pick B", CHANNEL).await;

        assert!(second.is_err(), "{second:?}");
        assert_eq!(run_rows(pool).await, rows);
        assert_eq!(node_run_id(pool, NODE).await, Some(run_id.to_string()));
    }

    #[tokio::test]
    async fn a_decision_with_no_tree_is_answered_and_starts_nothing() {
        let state = live_agent_state().await;
        let pool = &state.pool;
        sqlx::query(
            "INSERT INTO decisions (id, tree_id, node_id, question, answer, origin, at, status)
             VALUES ('treeless', NULL, NULL, ?, NULL, NULL, ?, 'open')",
        )
        .bind(QUESTION)
        .bind(CREATED)
        .execute(pool)
        .await
        .unwrap();

        let answered = answer_and_relaunch(&state, "treeless", ANSWER, CHANNEL)
            .await
            .unwrap();

        assert_eq!(answered.relaunched, None);
        assert_eq!(answered.decision.status, "answered");
        assert_eq!(answered.decision.answer.as_deref(), Some(ANSWER));
        assert_eq!(run_rows(pool).await, 0);
    }

    // ----- project_id on POST /trees -----

    fn tree_body() -> Value {
        json!({
            "id": TREE,
            "goal": "Ship it",
            "feature_branch": FEATURE,
            "project_root": NOWHERE,
        })
    }

    async fn rows_of(pool: &SqlitePool, count_query: &'static str) -> i64 {
        sqlx::query_scalar(count_query)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Every node close of such a tree would answer 422 for want of a project, so it is refused
    /// where the owner can still fix it.
    #[tokio::test]
    async fn post_tree_without_project_id_is_400_and_stores_nothing() {
        let state = live_agent_state().await;
        let app = tree_app(&state);
        let absent = tree_body();
        let mut null = tree_body();
        null["project_id"] = Value::Null;
        let mut blank = tree_body();
        blank["project_id"] = json!("   ");

        for body in [absent, null, blank] {
            let (status, answer) = post(&app, "/trees", body.clone()).await;

            assert_eq!(status, StatusCode::BAD_REQUEST, "{body:?}: {answer:?}");
            let error = answer["error"].as_str().expect("an error text");
            assert!(error.contains("project_id"), "{error:?}");
        }
        assert_eq!(
            rows_of(&state.pool, "SELECT COUNT(*) FROM tree_jobs").await,
            0
        );
        assert_eq!(
            rows_of(&state.pool, "SELECT COUNT(*) FROM work_nodes").await,
            0
        );
    }

    #[tokio::test]
    async fn post_tree_with_a_project_id_opens_it() {
        let state = live_agent_state().await;
        let mut body = tree_body();
        body["project_id"] = json!(PROJECT);

        let (status, answer) = post(&tree_app(&state), "/trees", body).await;

        assert_eq!(status, StatusCode::OK, "{answer:?}");
        let job = store::load(&state.pool, TREE).await.unwrap().unwrap();
        assert_eq!(job.project_id.as_deref(), Some(PROJECT));
        assert_eq!(
            node_tree_job(&state.pool, &job.root_node_id)
                .await
                .as_deref(),
            Some(TREE)
        );
    }
}
