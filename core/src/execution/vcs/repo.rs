//! Repository identity and resolution for VCS requests.

use super::*;

/// Who is asking, which decides whether the request needs a human's sign-off before it may queue.
///
/// A human's order in an interactive session already is the approval — asking again two seconds
/// later is friction with no safety gain. An autonomous run or job's request is not: nothing else
/// in the system has consented to it yet, so it waits. The queue itself never decides consent, only
/// ordering and mutual exclusion — this is where consent, already decided elsewhere, is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Human,
    // **A person's own editor session, asking through its safety hook** — `hooks::session_git_decision`.
    //
    // This said "nothing maps to this, and that is a decision rather than an omission", and it
    // survived on the argument that the schema outranks the mapping. The caller it was waiting for
    // is the one it described almost exactly: *"a credential belonging to an external session in its
    // own right, distinct from both the desktop app's control token and a run's"*. What arrived is
    // one step off that description and the difference is worth keeping straight — the hook presents
    // the control token, so the credential is not a session's own; what is a session's own is the
    // ASKING. `Human` would have been a lie of a readable kind: nobody clicked anything.
    //
    // It stays distinct from `Human` for the reason the audit exists. A `human` row means a person
    // acted in the app; a `shell` row means a person's agent tried to act and was redirected here
    // instead. Those are different events and the queue is the only place that records the second.
    //
    // The Tauri app is NOT this, despite owning the `shell/` directory: it holds the control token
    // and arrives through `vcs_origin` as `Human`, which is why the name was free.
    Shell,
    /// The daemon acting on the owner's standing order — today only the post-merge gate's revert,
    /// which exists because the owner turned `revert_on_red` on. Turning the switch on is the
    /// consent, so the request needs no approval of its own, and it is not recorded as `Human`
    /// because nobody clicked anything.
    Daemon,
    Run(i64),
    // Constructed by Chunk 4, when jobs submit requests of their own.
    Job(i64),
}
impl Origin {
    /// The exact spelling the `origin` column's CHECK constraint accepts — do not invent others.
    pub(super) fn as_str(self) -> &'static str {
        match self {
            Origin::Human => "human",
            Origin::Shell => "shell",
            Origin::Daemon => "daemon",
            Origin::Run(_) => "run",
            Origin::Job(_) => "job",
        }
    }

    /// Human and shell requests carry their own approval; run and job requests are autonomous and
    /// have not been approved by anything yet.
    pub(super) fn needs_approval(self) -> bool {
        matches!(self, Origin::Run(_) | Origin::Job(_))
    }

    /// Whether this origin may queue `op`. The daemon-only kinds (`DAEMON_OP_KINDS`) are refused to
    /// the autonomous origins: a run or a job asking for a revert would be an agent undoing a merge.
    pub(super) fn may_queue(self, op: &Op) -> bool {
        !(matches!(self, Origin::Run(_) | Origin::Job(_)) && DAEMON_OP_KINDS.contains(&op.kind()))
    }

    /// `run_id` is populated only for `Origin::Run`. A job id written into a column named
    /// `run_id` would silently mislabel it as a run — job ids and run ids come from different
    /// sequences and would collide (see `worktree::Owner::feed_run_id`'s doc comment for the same
    /// mistake made once already). A `job_id` column arrives once jobs actually submit requests,
    /// which is not this chunk.
    pub(super) fn run_id(self) -> Option<i64> {
        match self {
            Origin::Run(id) => Some(id),
            _ => None,
        }
    }
}
/// A project resolved to the repository it names, with the key the queue locks on.
///
/// Private fields with one production constructor, because the defect this type exists to kill was
/// two fields allowed to disagree: a caller that could set `key` and `root` independently could take
/// the lock on one repository and run git in another, and the row would look entirely ordinary.
#[derive(Debug, Clone)]
pub struct ResolvedRepo {
    pub(super) project_id: String,
    pub(super) root: String,
    pub(super) key: String,
}
impl ResolvedRepo {
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn root(&self) -> &str {
        &self.root
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    /// Tests build repositories that do not exist on disk: what most of them exercise is the SQL,
    /// and making each one create a real git repository would test git twice and slow the suite.
    /// `resolve_repo` is the only constructor compiled into the daemon.
    #[cfg(any(test, feature = "testkit"))]
    pub fn synthetic(project_id: &str, root: &str, key: &str) -> Self {
        Self {
            project_id: project_id.to_owned(),
            root: root.to_owned(),
            key: key.to_owned(),
        }
    }
}
/// Why a project could not be resolved to a repository.
///
/// Two failure arms rather than one string because the HTTP layer answers them differently and a
/// caller deserves to know which happened: an unknown project is the caller naming something that is
/// not there, and a bad root is the daemon's own recorded state being wrong.
#[derive(Debug)]
pub enum ResolveError {
    UnknownProject,
    NotARepository(String),
    Database(sqlx::Error),
}

// No `Display`: the one consumer, `http.rs`, destructures every arm and formats the inner value
// itself, so a `Display` here would be dead code that clippy cannot see — trait impls are exempt
// from dead-code analysis. Whoever gains a caller that wants to print one whole writes it then.

/// The project a directory belongs to, for a caller that knows only where it is standing.
///
/// `resolve_repo` goes the other way, from a name the caller already had. This is for the caller that
/// has no name at all: an interactive session's safety hook, which knows its `cwd` and nothing else.
/// Both ends meet at the same `ResolvedRepo`, because this returns a project id and hands it straight
/// back to `resolve_repo` rather than building one — the type has one production constructor for a
/// reason, and a second would be free to let `root` and `key` disagree.
///
/// **The match is on the repository, not on the path**, and that is the whole reason a session in a
/// linked worktree resolves at all. `C:\Projects\nucleos-assuntos` is not under `C:\Projects\nucleos`
/// and no prefix test would ever relate them; what relates them is `--git-common-dir`, which both
/// answer identically. Measured on this repo: thirteen worktrees, thirteen different top levels, one
/// common dir. That is also precisely why the queue serialises across them for free — the key it
/// locks on IS that common dir, so `one_running_vcs_request_per_repo` was already counting every
/// session in every worktree before any of them could reach it.
///
/// The parent of the common dir is the main working tree's root. That holds for every repository this
/// queue can serve and fails only for a bare one, which `repo_key` has already refused by here.
pub async fn project_for_worktree(
    pool: &sqlx::SqlitePool,
    worktree_root: &std::path::Path,
    deadline: std::time::Instant,
) -> Result<String, String> {
    let key = crate::execution::git_exec::repo_key(worktree_root, deadline).await?;
    let main_root = std::path::Path::new(&key)
        .parent()
        .ok_or_else(|| format!("{key} has no parent, so it names no working tree"))?
        .to_owned();

    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT project_id, project_root FROM autopilot_state WHERE project_root IS NOT NULL
         ORDER BY project_id",
    )
    .fetch_all(pool)
    .await
    .map_err(|error| format!("could not read the project roster: {error}"))?;

    for (project_id, root) in &rows {
        // Canonicalised on both sides rather than compared as written. `canonical` returns a Windows
        // verbatim path and a recorded root is whatever a person typed, so the two spellings reach
        // one directory and compare unequal — the failure `canonical`'s own doc comment warns about.
        // A root that has since been deleted canonicalises to an error and is skipped, not fatal:
        // one stale roster row must not stop the rest of the roster from answering.
        if let Ok(canonical_root) =
            crate::execution::git_exec::canonical(std::path::Path::new(root)).await
            && std::path::Path::new(&canonical_root) == main_root
        {
            return Ok(project_id.clone());
        }
    }

    Err(format!(
        "no project on the roster is rooted at {} — the queue takes work for projects it knows, and \
         a project in `off` mode has its root cleared, so that is the first thing to check",
        main_root.display()
    ))
}
/// The single production path from a project id to a repository the queue may lock.
///
/// Both halves are needed: `autopilot_state` is the only place a root is recorded, and git is what
/// makes two projects sharing a repository share a lock. It runs a subprocess, so it is neither free
/// nor infallible — that is the trade against keying on a label, which is what it replaces.
pub async fn resolve_repo(
    pool: &sqlx::SqlitePool,
    project_id: &str,
) -> Result<ResolvedRepo, ResolveError> {
    let root = crate::inspect::project_root(pool, project_id)
        .await
        .map_err(ResolveError::Database)?
        .ok_or(ResolveError::UnknownProject)?;
    let deadline = std::time::Instant::now() + crate::execution::git_exec::OPERATION_TIMEOUT;
    let key = crate::execution::git_exec::repo_key(std::path::Path::new(&root), deadline)
        .await
        .map_err(ResolveError::NotARepository)?;

    Ok(ResolvedRepo {
        project_id: project_id.to_owned(),
        root,
        key,
    })
}
