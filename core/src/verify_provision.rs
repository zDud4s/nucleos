//! Makes the `verify` box available to the IDE worktrees of a project (spec 2026-10-05 section
//! 4.7.1; F2c-2).
//!
//! With a project's `ide_verify` switch on, [`reconcile`] writes a daemon-owned `.mcp.json` and the
//! Claude Code approval (`enabledMcpjsonServers`) into each of its IDE worktrees and hides both
//! through `info/exclude`; with it off, [`reconcile`] removes exactly that and nothing else. It only
//! runs when `POST /projects/{id}/ide-verify` is called: with the switch off the daemon does not
//! even walk worktrees on its own.
//!
//! What was written is recorded in `<absolute-git-dir>/nucleos-ide-verify.json`, the worktree's
//! private git directory, so the record never enters the tree and dies with the worktree. It is
//! written BEFORE the files, so a crash mid-write still leaves the switch able to clean up. A
//! `.mcp.json` is removed only when its bytes are the ones recorded; anything the user wrote or
//! edited is left alone and reported.
//!
//! Nothing here runs a git command that rewrites the index (`ls-files`, `rev-parse` and
//! `worktree list` only), so a pass with nothing to do leaves `.git` byte-identical.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use axum::Json;
use axum::extract::{Extension, Path as AxumPath, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqlitePool;

use crate::auth::Scope;
use crate::git_exec::{self, OPERATION_TIMEOUT};
use crate::state::AppState;
use crate::verify::Caller;
use crate::{autopilot, inspect};

const MCP_FILE: &str = ".mcp.json";
const CLAUDE_DIR: &str = ".claude";
const SETTINGS_FILE: &str = ".claude/settings.local.json";
const SERVERS_KEY: &str = "enabledMcpjsonServers";
const SERVER_NAME: &str = "nucleos";
const PROVENANCE_FILE: &str = "nucleos-ide-verify.json";
const BLOCK_START: &str = "# nucleos ide-verify: daemon-owned, removed when the switch goes off";
const BLOCK_END: &str = "# end nucleos ide-verify";

/// One reconcile at a time: two overlapping passes would race on the same provenance and exclude
/// files.
static RECONCILE_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The MCP config an IDE session in `worktree` is launched with: the worktree box, no token and no
/// `env` (the box falls back to the Credential Manager).
pub fn worktree_mcp_config(exe: &str, worktree: &str) -> Value {
    serde_json::json!({
        "mcpServers": {
            SERVER_NAME: {
                "type": "stdio",
                "command": exe,
                "args": ["--mcp-tools", "--box", "worktree", "--worktree", worktree]
            }
        }
    })
}

/// What a reconcile did to one worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProvisionState {
    /// The entry and the approval are in place.
    Provisioned,
    /// What the daemon wrote was taken away.
    Removed,
    /// Left as it was, and the report says why.
    NotProvisioned,
    /// The switch is off and the daemon never wrote here: no write happened.
    Untouched,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorktreeReport {
    pub path: String,
    pub state: ProvisionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SwitchAnswer {
    pub project: String,
    pub enabled: bool,
    pub worktrees: Vec<WorktreeReport>,
}

#[derive(Debug, Deserialize)]
pub struct IdeVerifyArgs {
    pub enabled: bool,
}

/// What the daemon wrote into one worktree, enough to undo exactly that.
#[derive(Debug, Default, Serialize, Deserialize)]
struct Provenance {
    /// The exact content of the `.mcp.json` the daemon wrote.
    mcp_bytes: String,
    settings_created: bool,
    claude_dir_created: bool,
    servers_key_added: bool,
    nucleos_added: bool,
}

fn report(path: &str, state: ProvisionState, reason: Option<String>) -> WorktreeReport {
    WorktreeReport {
        path: path.to_owned(),
        state,
        reason,
    }
}

async fn git(repo: &Path, args: &[&str]) -> Result<git_exec::CommandResult, String> {
    let args: Vec<&OsStr> = args.iter().map(|arg| OsStr::new(*arg)).collect();
    git_exec::run_git(repo, &args, OPERATION_TIMEOUT).await
}

/// The project's IDE worktrees: every listed worktree except detached ones (integration) and those
/// a daemon run or job owns (an active `worktrees` row).
async fn ide_worktrees(
    pool: &SqlitePool,
    project_id: &str,
    root: &Path,
) -> Result<Vec<String>, String> {
    let listed = git(root, &["worktree", "list", "--porcelain"]).await?;
    if !listed.succeeded() {
        return Err(format!(
            "could not list {project_id}'s worktrees: {}",
            listed.output_tail
        ));
    }
    let owned_rows: Vec<String> = sqlx::query_scalar(
        "SELECT path FROM worktrees WHERE project_id = ? AND removed_at IS NULL",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
    .map_err(|error| format!("could not read {project_id}'s worktree rows: {error}"))?;
    let mut owned = HashSet::new();
    for row in owned_rows {
        if let Ok(key) = git_exec::canonical(Path::new(&row)).await {
            owned.insert(key);
        }
    }

    let mut out = Vec::new();
    for block in listed.stdout.replace("\r\n", "\n").split("\n\n") {
        let mut path = None;
        let mut skip = false;
        for line in block.lines() {
            if let Some(found) = line.strip_prefix("worktree ") {
                path = Some(found.to_owned());
            } else if line == "detached" || line == "bare" {
                skip = true;
            }
        }
        let Some(path) = path else { continue };
        if skip {
            continue;
        }
        // A listed worktree whose directory is gone cannot be written to.
        let Ok(key) = git_exec::canonical(Path::new(&path)).await else {
            continue;
        };
        if owned.contains(&key) {
            continue;
        }
        out.push(path);
    }
    Ok(out)
}

async fn is_tracked(worktree: &Path, rel: &str) -> Result<bool, String> {
    let out = git(worktree, &["--literal-pathspecs", "ls-files", "--", rel]).await?;
    if !out.succeeded() {
        return Err(format!("git ls-files {rel} failed: {}", out.output_tail));
    }
    Ok(!out.stdout.trim().is_empty())
}

async fn git_dir(worktree: &Path) -> Result<PathBuf, String> {
    let out = git(worktree, &["rev-parse", "--absolute-git-dir"]).await?;
    if !out.succeeded() || out.stdout.trim().is_empty() {
        return Err(format!(
            "could not resolve the git directory: {}",
            out.output_tail
        ));
    }
    Ok(PathBuf::from(out.stdout.trim()))
}

/// The directory `info/exclude` lives in: shared by every worktree of the repository.
async fn common_dir(worktree: &Path) -> Result<PathBuf, String> {
    let out = git(worktree, &["rev-parse", "--git-common-dir"]).await?;
    if !out.succeeded() || out.stdout.trim().is_empty() {
        return Err(format!(
            "could not resolve the common git directory: {}",
            out.output_tail
        ));
    }
    // Older git answers relatively, and relative to the `-C` directory.
    let reported = PathBuf::from(out.stdout.trim());
    Ok(if reported.is_absolute() {
        reported
    } else {
        worktree.join(reported)
    })
}

async fn read_optional(path: &Path) -> Result<Option<String>, String> {
    match tokio::fs::read_to_string(path).await {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("could not read {}: {error}", path.display())),
    }
}

async fn write(path: &Path, text: &str) -> Result<(), String> {
    tokio::fs::write(path, text)
        .await
        .map_err(|error| format!("could not write {}: {error}", path.display()))
}

fn block_text() -> String {
    format!("{BLOCK_START}\n/{MCP_FILE}\n/{SETTINGS_FILE}\n{BLOCK_END}\n")
}

/// Adds the marked block to `info/exclude` once; a no-op (no write) when it is already there.
async fn ensure_exclude_block(common: &Path) -> Result<(), String> {
    let exclude = common.join("info").join("exclude");
    let existing = read_optional(&exclude).await?.unwrap_or_default();
    if existing.lines().any(|line| line == BLOCK_START) {
        return Ok(());
    }
    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(&block_text());
    if let Some(parent) = exclude.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
    }
    write(&exclude, &updated).await
}

/// Removes exactly the marked block; rewrites the file only when the block is present.
async fn remove_exclude_block(common: &Path) -> Result<(), String> {
    let exclude = common.join("info").join("exclude");
    let Some(existing) = read_optional(&exclude).await? else {
        return Ok(());
    };
    let Some(start) = existing
        .match_indices(BLOCK_START)
        .map(|(at, _)| at)
        .find(|&at| at == 0 || existing.as_bytes()[at - 1] == b'\n')
    else {
        return Ok(());
    };
    let Some(end_at) = existing[start..].find(BLOCK_END) else {
        return Ok(());
    };
    let mut end = start + end_at + BLOCK_END.len();
    if existing[end..].starts_with("\r\n") {
        end += 2;
    } else if existing[end..].starts_with('\n') {
        end += 1;
    }
    let mut updated = String::with_capacity(existing.len());
    updated.push_str(&existing[..start]);
    updated.push_str(&existing[end..]);
    write(&exclude, &updated).await
}

/// The settings after the merge, and what the merge added. `Err` is a reason to leave the file be.
struct Merge {
    value: Value,
    created: bool,
    key_added: bool,
    nucleos_added: bool,
}

fn merge_settings(existing: Option<&str>) -> Result<Merge, String> {
    let Some(text) = existing else {
        return Ok(Merge {
            value: serde_json::json!({ SERVERS_KEY: [SERVER_NAME] }),
            created: true,
            key_added: true,
            nucleos_added: true,
        });
    };
    let mut value: Value = serde_json::from_str(text)
        .map_err(|error| format!("{SETTINGS_FILE} is not valid JSON ({error}); left untouched"))?;
    let Some(object) = value.as_object_mut() else {
        return Err(format!(
            "{SETTINGS_FILE} is not a JSON object; left untouched"
        ));
    };
    let mut key_added = false;
    let mut nucleos_added = false;
    match object.get_mut(SERVERS_KEY) {
        None => {
            object.insert(SERVERS_KEY.to_owned(), serde_json::json!([SERVER_NAME]));
            key_added = true;
            nucleos_added = true;
        }
        Some(Value::Array(list)) => {
            if !list.iter().any(|item| item == SERVER_NAME) {
                list.push(Value::from(SERVER_NAME));
                nucleos_added = true;
            }
        }
        Some(_) => {
            return Err(format!(
                "{SETTINGS_FILE}: {SERVERS_KEY} is not a list; left untouched"
            ));
        }
    }
    Ok(Merge {
        value,
        created: false,
        key_added,
        nucleos_added,
    })
}

fn pretty(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_owned());
    text.push('\n');
    text
}

async fn read_provenance(git_dir: &Path) -> Result<Option<Provenance>, String> {
    let path = git_dir.join(PROVENANCE_FILE);
    let Some(text) = read_optional(&path).await? else {
        return Ok(None);
    };
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|error| format!("{} is unreadable ({error}); left untouched", path.display()))
}

/// Provisions one worktree. `Ok` carries the outcome, including a refusal that is not an error.
async fn provision(path: &str, exe: &str) -> Result<WorktreeReport, String> {
    let wt = Path::new(path);
    for rel in [MCP_FILE, SETTINGS_FILE] {
        if is_tracked(wt, rel).await? {
            return Ok(report(
                path,
                ProvisionState::NotProvisioned,
                Some(format!("{rel} is tracked by git; nothing written")),
            ));
        }
    }
    let git_dir = git_dir(wt).await?;
    let previous = read_provenance(&git_dir).await?;
    let mcp_path = wt.join(MCP_FILE);
    let existing_mcp = read_optional(&mcp_path).await?;
    if let Some(found) = &existing_mcp {
        let ours = previous
            .as_ref()
            .is_some_and(|prov| &prov.mcp_bytes == found);
        if !ours {
            return Ok(report(
                path,
                ProvisionState::NotProvisioned,
                Some(format!(
                    "{MCP_FILE} exists and is not the daemon's; left untouched"
                )),
            ));
        }
    }
    let settings_path = wt.join(SETTINGS_FILE);
    let settings_text = read_optional(&settings_path).await?;
    let merge = match merge_settings(settings_text.as_deref()) {
        Ok(merge) => merge,
        Err(reason) => return Ok(report(path, ProvisionState::NotProvisioned, Some(reason))),
    };
    let claude_dir = wt.join(CLAUDE_DIR);
    let claude_dir_created = !claude_dir.exists();

    let desired = pretty(&worktree_mcp_config(exe, path));
    let old = previous.unwrap_or_default();
    let provenance = Provenance {
        mcp_bytes: desired.clone(),
        settings_created: old.settings_created || merge.created,
        claude_dir_created: old.claude_dir_created || claude_dir_created,
        servers_key_added: old.servers_key_added || merge.key_added,
        nucleos_added: old.nucleos_added || merge.nucleos_added,
    };

    // The record first, so a crash between here and the last write is still undoable.
    let provenance_text = pretty(&serde_json::to_value(&provenance).map_err(|e| e.to_string())?);
    let provenance_path = git_dir.join(PROVENANCE_FILE);
    if read_optional(&provenance_path).await?.as_deref() != Some(provenance_text.as_str()) {
        write(&provenance_path, &provenance_text).await?;
    }
    if existing_mcp.as_deref() != Some(desired.as_str()) {
        write(&mcp_path, &desired).await?;
    }
    if merge.created || merge.key_added || merge.nucleos_added {
        tokio::fs::create_dir_all(&claude_dir)
            .await
            .map_err(|error| format!("could not create {}: {error}", claude_dir.display()))?;
        write(&settings_path, &pretty(&merge.value)).await?;
    }
    Ok(report(path, ProvisionState::Provisioned, None))
}

/// Undoes what the daemon recorded in one worktree.
async fn unprovision(path: &str) -> Result<WorktreeReport, String> {
    let wt = Path::new(path);
    let git_dir = git_dir(wt).await?;
    let provenance = match read_provenance(&git_dir).await {
        Ok(Some(provenance)) => provenance,
        Ok(None) => return Ok(report(path, ProvisionState::Untouched, None)),
        Err(reason) => return Ok(report(path, ProvisionState::NotProvisioned, Some(reason))),
    };
    let mut state = ProvisionState::Removed;
    let mut reason = None;

    let mcp_path = wt.join(MCP_FILE);
    if let Some(found) = read_optional(&mcp_path).await? {
        if found == provenance.mcp_bytes {
            tokio::fs::remove_file(&mcp_path)
                .await
                .map_err(|error| format!("could not remove {}: {error}", mcp_path.display()))?;
        } else {
            state = ProvisionState::NotProvisioned;
            reason = Some(format!(
                "{MCP_FILE} was edited since the daemon wrote it; kept"
            ));
        }
    }

    let settings_path = wt.join(SETTINGS_FILE);
    if (provenance.nucleos_added || provenance.servers_key_added || provenance.settings_created)
        && let Some(text) = read_optional(&settings_path).await?
    {
        match serde_json::from_str::<Value>(&text) {
            Ok(mut value) if value.is_object() => {
                let object = value.as_object_mut().expect("checked above");
                if provenance.nucleos_added
                    && let Some(Value::Array(list)) = object.get_mut(SERVERS_KEY)
                {
                    list.retain(|item| item != SERVER_NAME);
                }
                if provenance.servers_key_added
                    && object
                        .get(SERVERS_KEY)
                        .is_some_and(|v| v.as_array().is_some_and(Vec::is_empty))
                {
                    object.remove(SERVERS_KEY);
                }
                if provenance.settings_created && object.is_empty() {
                    tokio::fs::remove_file(&settings_path)
                        .await
                        .map_err(|error| {
                            format!("could not remove {}: {error}", settings_path.display())
                        })?;
                    if provenance.claude_dir_created {
                        // Fails, harmlessly, when the directory holds anything else.
                        let _ = tokio::fs::remove_dir(wt.join(CLAUDE_DIR)).await;
                    }
                } else {
                    write(&settings_path, &pretty(&value)).await?;
                }
            }
            _ => {
                state = ProvisionState::NotProvisioned;
                reason.get_or_insert_with(|| {
                    format!("{SETTINGS_FILE} is no longer a JSON object; kept")
                });
            }
        }
    }

    let provenance_path = git_dir.join(PROVENANCE_FILE);
    tokio::fs::remove_file(&provenance_path)
        .await
        .map_err(|error| format!("could not remove {}: {error}", provenance_path.display()))?;
    Ok(report(path, state, reason))
}

/// Brings every IDE worktree of `project_id` in line with its switch.
pub(crate) async fn reconcile(
    pool: &SqlitePool,
    project_id: &str,
    exe: &str,
) -> Result<Vec<WorktreeReport>, String> {
    let _serialised = RECONCILE_LOCK.lock().await;
    let enabled = autopilot::ide_verify_enabled(pool, project_id)
        .await
        .map_err(|error| format!("could not read the IDE verify switch: {error}"))?;
    let root = inspect::project_root(pool, project_id)
        .await
        .map_err(|error| format!("could not read the project root: {error}"))?
        .ok_or_else(|| format!("project {project_id} has no root recorded"))?;
    let root = Path::new(&root);
    let worktrees = ide_worktrees(pool, project_id, root).await?;

    let mut reports = Vec::with_capacity(worktrees.len());
    if enabled && !worktrees.is_empty() {
        ensure_exclude_block(&common_dir(root).await?).await?;
    }
    for path in &worktrees {
        let outcome = if enabled {
            provision(path, exe).await
        } else {
            unprovision(path).await
        };
        reports.push(match outcome {
            Ok(done) => done,
            Err(error) => report(path, ProvisionState::NotProvisioned, Some(error)),
        });
    }
    if !enabled {
        remove_exclude_block(&common_dir(root).await?).await?;
    }
    Ok(reports)
}

/// Sets the switch and reconciles. Owner only: the check comes before any database or file work.
pub(crate) async fn switch(
    pool: &SqlitePool,
    scope: &Scope,
    project: &str,
    enabled: bool,
    exe: &str,
) -> Result<SwitchAnswer, (StatusCode, String)> {
    if Caller::from_scope(scope) != Some(Caller::Owner) {
        return Err((
            StatusCode::FORBIDDEN,
            "only the owner's key can switch IDE verify".to_owned(),
        ));
    }
    let matched = autopilot::set_ide_verify(pool, project, enabled)
        .await
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not set the IDE verify switch of {project}: {error}"),
            )
        })?;
    if !matched {
        return Err((
            StatusCode::NOT_FOUND,
            format!("no project {project} is rostered"),
        ));
    }
    let worktrees = reconcile(pool, project, exe)
        .await
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error))?;
    Ok(SwitchAnswer {
        project: project.to_owned(),
        enabled,
        worktrees,
    })
}

/// `POST /projects/{id}/ide-verify`: control or admin key only.
pub async fn post_project_ide_verify(
    State(state): State<AppState>,
    AxumPath(id): AxumPath<String>,
    Extension(scope): Extension<Scope>,
    Json(args): Json<IdeVerifyArgs>,
) -> Result<Json<SwitchAnswer>, (StatusCode, String)> {
    let exe = std::env::current_exe()
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("could not resolve the daemon executable: {error}"),
            )
        })?
        .to_string_lossy()
        .into_owned();
    switch(&state.pool, &scope, &id, args.enabled, &exe)
        .await
        .map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Scope;
    use axum::http::StatusCode;
    use sqlx::SqlitePool;
    use std::collections::BTreeMap;
    use std::path::{Path, PathBuf};

    const EXE: &str = "/opt/nucleos/nucleos-core";
    const MCP: &str = ".mcp.json";
    const SETTINGS: &str = ".claude/settings.local.json";

    /// Runs git in `dir`, asserts it succeeded, and returns its trimmed stdout.
    fn git_in(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "core.autocrlf=false",
            ])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    /// A one-commit git repository.
    fn repo() -> tempfile::TempDir {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join("a.txt"), "a\n").unwrap();
        git_in(repo.path(), &["init", "-q", "-b", "main"]);
        git_in(repo.path(), &["add", "-A"]);
        git_in(repo.path(), &["commit", "-q", "-m", "one"]);
        repo
    }

    /// A linked worktree of `repo`, in a temp directory of its own. The returned guard owns the
    /// directory; the worktree is `<guard>/wt`.
    fn linked(repo: &Path, name: &str) -> (tempfile::TempDir, PathBuf) {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join(name);
        git_in(
            repo,
            &["worktree", "add", "-q", &path.to_string_lossy(), "-b", name],
        );
        (parent, path)
    }

    /// A detached linked worktree, the shape an integration worktree has.
    fn detached(repo: &Path, name: &str) -> (tempfile::TempDir, PathBuf) {
        let parent = tempfile::tempdir().unwrap();
        let path = parent.path().join(name);
        git_in(
            repo,
            &["worktree", "add", "-q", "--detach", &path.to_string_lossy()],
        );
        (parent, path)
    }

    /// A pool with `repo` rostered as project `alpha`, its IDE verify switch as given.
    async fn pool_with(repo: &Path, ide_verify: bool) -> SqlitePool {
        let pool = crate::testdb::fresh_pool().await;
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root, ide_verify) \
             VALUES ('alpha', 'active', ?, ?)",
        )
        .bind(repo.to_string_lossy().into_owned())
        .bind(i64::from(ide_verify))
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    async fn switch_is(pool: &SqlitePool) -> bool {
        crate::autopilot::ide_verify_enabled(pool, "alpha")
            .await
            .unwrap()
    }

    /// Every file and directory under `root` with its bytes (directories map to an empty vector
    /// under a key ending in `/`). `.git` is included when it is a directory.
    fn walk(root: &Path) -> BTreeMap<String, Vec<u8>> {
        fn go(base: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
            for entry in std::fs::read_dir(dir).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                let rel = path
                    .strip_prefix(base)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if entry.file_type().unwrap().is_dir() {
                    out.insert(format!("{rel}/"), Vec::new());
                    go(base, &path, out);
                } else {
                    out.insert(rel, std::fs::read(&path).unwrap());
                }
            }
        }
        let mut out = BTreeMap::new();
        go(root, root, &mut out);
        out
    }

    fn status(dir: &Path) -> String {
        git_in(dir, &["status", "--porcelain"])
    }

    fn json_of(path: &Path) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
    }

    async fn same_dir(a: &str, b: &Path) -> bool {
        crate::git_exec::canonical(Path::new(a)).await.unwrap()
            == crate::git_exec::canonical(b).await.unwrap()
    }

    /// The report whose path names `dir`.
    async fn report_for<'a>(reports: &'a [WorktreeReport], dir: &Path) -> &'a WorktreeReport {
        for report in reports {
            if same_dir(&report.path, dir).await {
                return report;
            }
        }
        panic!("no report for {dir:?} in {} reports", reports.len());
    }

    #[tokio::test]
    async fn with_the_switch_off_nothing_is_written_anywhere() {
        let repo = repo();
        let (_keep, wt) = linked(repo.path(), "wt");
        let pool = pool_with(repo.path(), false).await;

        // `status` can refresh the index, so settle it before taking the snapshot.
        let status_main = status(repo.path());
        let status_wt = status(&wt);
        let main_before = walk(repo.path());
        let wt_before = walk(&wt);

        let reports = reconcile(&pool, "alpha", EXE).await.unwrap();
        assert!(
            reports
                .iter()
                .all(|r| matches!(r.state, ProvisionState::Untouched)),
            "a never-on project reports every worktree as untouched"
        );
        let answer = switch(&pool, &Scope::Control, "alpha", false, EXE)
            .await
            .unwrap();
        assert!(!answer.enabled);

        assert_eq!(walk(repo.path()), main_before, "main tree and git dir");
        assert_eq!(walk(&wt), wt_before, "linked worktree");
        assert_eq!(status(repo.path()), status_main);
        assert_eq!(status(&wt), status_wt);
        assert!(!repo.path().join(MCP).exists());
        assert!(!wt.join(MCP).exists());
        assert!(!switch_is(&pool).await);
    }

    #[test]
    fn the_entry_launches_the_worktree_box_and_carries_no_token() {
        let config = worktree_mcp_config(EXE, "/w/a");

        let server = &config["mcpServers"]["nucleos"];
        assert_eq!(server["type"], "stdio");
        assert_eq!(server["command"], EXE);
        assert_eq!(
            server["args"],
            serde_json::json!(["--mcp-tools", "--box", "worktree", "--worktree", "/w/a"])
        );
        assert!(server.get("env").is_none(), "no env key at all");
        assert_eq!(config["mcpServers"].as_object().unwrap().len(), 1);
        let text = config.to_string().to_lowercase();
        assert!(!text.contains("token"), "{text}");
        assert!(!text.contains("bearer"), "{text}");
    }

    #[tokio::test]
    async fn switching_on_provisions_every_ide_worktree_and_leaves_status_clean() {
        let repo = repo();
        let (_keep, wt) = linked(repo.path(), "wt");
        let pool = pool_with(repo.path(), true).await;

        let reports = reconcile(&pool, "alpha", EXE).await.unwrap();

        assert_eq!(reports.len(), 2, "main checkout plus the linked worktree");
        for dir in [repo.path(), wt.as_path()] {
            let report = report_for(&reports, dir).await;
            assert!(
                matches!(report.state, ProvisionState::Provisioned),
                "{dir:?}: {:?}",
                report.reason
            );
            // The entry names this very worktree.
            let entry = json_of(&dir.join(MCP));
            let args = entry["mcpServers"]["nucleos"]["args"].as_array().unwrap();
            assert_eq!(
                args[..4],
                ["--mcp-tools", "--box", "worktree", "--worktree"]
            );
            assert!(same_dir(args[4].as_str().unwrap(), dir).await);
            assert_eq!(entry["mcpServers"]["nucleos"]["command"], EXE);
            // Claude Code's approval.
            assert_eq!(
                json_of(&dir.join(SETTINGS))["enabledMcpjsonServers"],
                serde_json::json!(["nucleos"])
            );
            assert_eq!(status(dir), "", "{dir:?} must stay clean");
        }
        // Provenance sits in each worktree's private git dir, never in the tree.
        for dir in [repo.path(), wt.as_path()] {
            let git_dir = PathBuf::from(git_in(dir, &["rev-parse", "--absolute-git-dir"]));
            assert!(git_dir.join("nucleos-ide-verify.json").is_file());
            assert!(!dir.join("nucleos-ide-verify.json").exists());
        }
        let exclude = std::fs::read_to_string(repo.path().join(".git/info/exclude")).unwrap();
        assert_eq!(
            exclude
                .matches("# nucleos ide-verify: daemon-owned, removed when the switch goes off")
                .count(),
            1
        );

        // Idempotent: a second pass changes nothing and does not stack a second block.
        let main_before = walk(repo.path());
        let wt_before = walk(&wt);
        let again = reconcile(&pool, "alpha", EXE).await.unwrap();
        assert!(
            again
                .iter()
                .all(|r| matches!(r.state, ProvisionState::Provisioned))
        );
        assert_eq!(walk(repo.path()), main_before);
        assert_eq!(walk(&wt), wt_before);
    }

    #[tokio::test]
    async fn a_tracked_mcp_json_is_never_touched() {
        let repo = repo();
        let theirs = "{\"mcpServers\":{\"theirs\":{\"command\":\"x\"}}}\n";
        std::fs::write(repo.path().join(MCP), theirs).unwrap();
        git_in(repo.path(), &["add", MCP]);
        git_in(repo.path(), &["commit", "-q", "-m", "track .mcp.json"]);
        let pool = pool_with(repo.path(), true).await;

        let reports = reconcile(&pool, "alpha", EXE).await.unwrap();

        assert_eq!(reports.len(), 1);
        assert!(
            matches!(reports[0].state, ProvisionState::NotProvisioned),
            "{:?}",
            reports[0].state
        );
        assert!(reports[0].reason.is_some(), "the reason is reported");
        assert_eq!(
            std::fs::read_to_string(repo.path().join(MCP)).unwrap(),
            theirs
        );
        assert!(!repo.path().join(SETTINGS).exists(), "nothing else written");
        assert!(
            !repo.path().join(".git/nucleos-ide-verify.json").exists(),
            "no provenance for a worktree we did not touch"
        );
        assert_eq!(status(repo.path()), "");
    }

    #[tokio::test]
    async fn a_foreign_untracked_mcp_json_is_left_alone() {
        let repo = repo();
        let theirs = "{\"mcpServers\":{\"theirs\":{\"command\":\"x\"}}}\n";
        std::fs::write(repo.path().join(MCP), theirs).unwrap();
        let pool = pool_with(repo.path(), true).await;

        let reports = reconcile(&pool, "alpha", EXE).await.unwrap();

        assert_eq!(reports.len(), 1);
        assert!(matches!(reports[0].state, ProvisionState::NotProvisioned));
        assert!(reports[0].reason.is_some());
        assert_eq!(
            std::fs::read_to_string(repo.path().join(MCP)).unwrap(),
            theirs
        );
        assert!(!repo.path().join(SETTINGS).exists());

        // Switching off must not claim it either.
        switch(&pool, &Scope::Control, "alpha", false, EXE)
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(repo.path().join(MCP)).unwrap(),
            theirs
        );
    }

    #[tokio::test]
    async fn existing_local_settings_are_merged_and_restored() {
        let repo = repo();
        let original = serde_json::json!({
            "permissions": { "allow": ["Bash(ls)"] },
            "enabledMcpjsonServers": ["other"],
            "theme": "dark"
        });
        std::fs::create_dir_all(repo.path().join(".claude")).unwrap();
        std::fs::write(
            repo.path().join(SETTINGS),
            serde_json::to_string_pretty(&original).unwrap(),
        )
        .unwrap();
        let pool = pool_with(repo.path(), true).await;

        let reports = reconcile(&pool, "alpha", EXE).await.unwrap();
        assert!(matches!(reports[0].state, ProvisionState::Provisioned));

        let merged = json_of(&repo.path().join(SETTINGS));
        assert_eq!(merged["permissions"], original["permissions"]);
        assert_eq!(merged["theme"], "dark");
        assert_eq!(
            merged["enabledMcpjsonServers"],
            serde_json::json!(["other", "nucleos"])
        );

        let off = switch(&pool, &Scope::Control, "alpha", false, EXE)
            .await
            .unwrap();
        assert!(matches!(off.worktrees[0].state, ProvisionState::Removed));
        assert_eq!(json_of(&repo.path().join(SETTINGS)), original);
        assert!(repo.path().join(".claude").is_dir(), "not ours to remove");
        assert!(!repo.path().join(MCP).exists());
    }

    #[tokio::test]
    async fn switching_off_removes_exactly_what_was_written() {
        let repo = repo();
        let (_keep, wt) = linked(repo.path(), "wt");
        let pool = pool_with(repo.path(), false).await;
        let status_main = status(repo.path());
        let status_wt = status(&wt);
        let main_before = walk(repo.path());
        let wt_before = walk(&wt);

        let on = switch(&pool, &Scope::Control, "alpha", true, EXE)
            .await
            .unwrap();
        assert!(on.enabled);
        assert_eq!(on.worktrees.len(), 2);
        assert_ne!(
            walk(repo.path()),
            main_before,
            "ON must have written something"
        );
        assert!(switch_is(&pool).await);

        let off = switch(&pool, &Scope::Control, "alpha", false, EXE)
            .await
            .unwrap();

        assert!(!off.enabled);
        assert!(!switch_is(&pool).await);
        for report in &off.worktrees {
            assert!(
                matches!(report.state, ProvisionState::Removed),
                "{}: {:?} {:?}",
                report.path,
                report.state,
                report.reason
            );
        }
        // Files, directories, provenance and `info/exclude`: byte-identical to before.
        assert_eq!(walk(repo.path()), main_before);
        assert_eq!(walk(&wt), wt_before);
        assert_eq!(status(repo.path()), status_main);
        assert_eq!(status(&wt), status_wt);
    }

    #[tokio::test]
    async fn an_edited_mcp_json_survives_switching_off() {
        let repo = repo();
        let pool = pool_with(repo.path(), true).await;
        reconcile(&pool, "alpha", EXE).await.unwrap();
        let mut edited = std::fs::read_to_string(repo.path().join(MCP)).unwrap();
        edited = edited.replace("\"nucleos\"", "\"nucleos\", \"mine\": {}, \"x\"");
        std::fs::write(repo.path().join(MCP), &edited).unwrap();

        let off = switch(&pool, &Scope::Control, "alpha", false, EXE)
            .await
            .unwrap();

        assert_eq!(off.worktrees.len(), 1);
        assert!(
            matches!(off.worktrees[0].state, ProvisionState::NotProvisioned),
            "{:?}",
            off.worktrees[0].state
        );
        assert!(off.worktrees[0].reason.is_some());
        assert_eq!(
            std::fs::read_to_string(repo.path().join(MCP)).unwrap(),
            edited,
            "the user's edit is never deleted"
        );
    }

    #[tokio::test]
    async fn daemon_owned_worktrees_are_never_provisioned() {
        let repo = repo();
        let (_k1, owned) = linked(repo.path(), "owned");
        let (_k2, integration) = detached(repo.path(), "integration");
        let pool = pool_with(repo.path(), true).await;
        sqlx::query(
            "INSERT INTO worktrees \
             (owner_kind, owner_id, project_id, project_root, path, branch, created_at) \
             VALUES ('run', 1, 'alpha', ?, ?, 'owned', '2026-10-08')",
        )
        .bind(repo.path().to_string_lossy().into_owned())
        .bind(owned.to_string_lossy().into_owned())
        .execute(&pool)
        .await
        .unwrap();

        let reports = reconcile(&pool, "alpha", EXE).await.unwrap();

        assert_eq!(
            reports.len(),
            1,
            "only the main checkout is an IDE worktree"
        );
        assert!(same_dir(&reports[0].path, repo.path()).await);
        assert!(matches!(reports[0].state, ProvisionState::Provisioned));
        assert!(repo.path().join(MCP).is_file());
        for skipped in [&owned, &integration] {
            assert!(!skipped.join(MCP).exists(), "{skipped:?}");
            assert!(!skipped.join(".claude").exists(), "{skipped:?}");
        }
    }

    #[tokio::test]
    async fn only_the_owner_can_flip_the_switch() {
        let repo = repo();
        let pool = pool_with(repo.path(), false).await;

        // A run key is refused before any database or filesystem work.
        let refused = switch(&pool, &Scope::Run(1), "alpha", true, EXE)
            .await
            .unwrap_err();
        assert_eq!(refused.0, StatusCode::FORBIDDEN);
        assert!(!switch_is(&pool).await, "the column is unchanged");
        assert!(!repo.path().join(MCP).exists());

        // An unknown project is a 404.
        let unknown = switch(&pool, &Scope::Control, "nobody", true, EXE)
            .await
            .unwrap_err();
        assert_eq!(unknown.0, StatusCode::NOT_FOUND);

        // The owner turns it on, then off, and each sets the column and reconciles.
        let on = switch(&pool, &Scope::Control, "alpha", true, EXE)
            .await
            .unwrap();
        assert!(on.enabled);
        assert_eq!(on.project, "alpha");
        assert!(switch_is(&pool).await);
        assert!(repo.path().join(MCP).is_file());

        let off = switch(&pool, &Scope::Control, "alpha", false, EXE)
            .await
            .unwrap();
        assert!(!off.enabled);
        assert!(!switch_is(&pool).await);
        assert!(!repo.path().join(MCP).exists());
    }
}
