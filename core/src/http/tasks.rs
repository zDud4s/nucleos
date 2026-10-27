//! The HTTP face of `task_claims`.
//!
//! Routes split in two. `/tasks/*` acts per worktree: the tools and the runs use it, and a run's
//! key crosses it because those are literal POST paths. `/projects/{id}/tasks*` is for the owner
//! only (hooks and the shell). A project that is not on the roster is "claims unavailable", never a
//! server error: the caller carries on as if nothing were claimed.

// The routes that use these helpers land in a later task of this chunk.
#![allow(dead_code)]

use std::collections::HashMap;
use std::time::{Duration, Instant};

use axum::http::StatusCode;
use serde_json::Value;

use crate::auth::{ApiTokenLevel, Scope};
use crate::task_claims::sessions;
use crate::task_claims::{Holder, HolderKind, Notice, TaskRow};

/// The result of resolving one `cwd`: `(project_id, canonical worktree root)`, or `None` when no
/// project on the roster is rooted there.
type Resolved = Option<(String, String)>;

/// `cwd -> project`, remembered between hook calls. Resolving costs a git subprocess and a
/// canonicalisation of every roster root (`vcs::project_for_worktree`), and the hook fires on every
/// tool call of every session. A refusal is cached too, but briefly: registering a project must
/// start working within a minute, and an unregistered one must not cost a subprocess per tool call.
#[derive(Default)]
pub(super) struct CwdCache {
    entries: HashMap<String, (Instant, Resolved)>,
}

impl CwdCache {
    pub(super) const POSITIVE_TTL: Duration = Duration::from_secs(600);
    pub(super) const NEGATIVE_TTL: Duration = Duration::from_secs(60);
    pub(super) const MAX_ENTRIES: usize = 256;

    /// `None` is a miss; `Some(None)` is a cached "not registered".
    pub(super) fn get(&self, cwd: &str, now: Instant) -> Option<Resolved> {
        let (at, resolved) = self.entries.get(cwd)?;
        let ttl = if resolved.is_some() { Self::POSITIVE_TTL } else { Self::NEGATIVE_TTL };
        (now.saturating_duration_since(*at) <= ttl).then(|| resolved.clone())
    }

    pub(super) fn put(&mut self, cwd: &str, resolved: Resolved, now: Instant) {
        if self.entries.len() >= Self::MAX_ENTRIES {
            self.entries.clear();
        }
        self.entries.insert(cwd.to_owned(), (now, resolved));
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.entries.len()
    }
}

/// How an agent can name a holder when it reads who has a task.
pub(super) fn holder_label(holder: &Holder) -> String {
    match holder.kind {
        HolderKind::Run => format!("run #{}", holder.id),
        HolderKind::Job => format!("job #{}", holder.id),
        HolderKind::Postgate => "the daemon (post-merge gate)".to_owned(),
        HolderKind::Verify => "the daemon (verify)".to_owned(),
        HolderKind::Session => format!("another session in {}", holder.worktree),
    }
}

/// The first seven characters of a sha, cut on a character boundary.
fn short(sha: &str) -> &str {
    sha.char_indices().nth(7).map_or(sha, |(end, _)| &sha[..end])
}

/// A notice as the agent reads it: what happened and what to do about it.
pub(super) fn render_notice(notice: &Notice) -> String {
    match notice {
        Notice::ClaimLost { task, new_holder } => format!(
            "Your claim on #{} `{}` is gone: it is now held by {}. Stop working on it; take another task or ask.",
            task.id,
            task.key,
            holder_label(new_holder),
        ),
        Notice::IncidentClosed { task, sha } => {
            let at = sha.as_deref().map(|sha| format!(" at {}", short(sha))).unwrap_or_default();
            format!(
                "Incident #{} `{}` was closed{at}: master is green again. If you were fixing it, stop; \
                 if your verify inherited it, rebase on master and verify again.",
                task.id, task.key,
            )
        }
        // Delivered by chunk 4; the same call to action as `IncidentClosed`, for the verify side.
        Notice::InheritedClosed { task, sha } => {
            let at = sha.as_deref().map(|sha| format!(" at {}", short(sha))).unwrap_or_default();
            format!(
                "Incident #{} `{}` was closed{at}: master is green again. Your verify inherited it: \
                 rebase on master and verify again.",
                task.id, task.key,
            )
        }
    }
}

/// One line of the "live incidents" list.
pub(super) fn render_incident(task: &TaskRow) -> String {
    let since = task
        .detail
        .get("red_since_sha")
        .and_then(Value::as_str)
        .map(|sha| format!(" since {}", short(sha)))
        .unwrap_or_default();
    let state = match task.holder() {
        Some(holder) => format!("being fixed by {}", holder_label(&holder)),
        None => "unclaimed".to_owned(),
    };
    format!("#{} {}{since}: {state}", task.id, task.key)
}

/// Where a request acts: the project, the worktree's canonical root, and the project's own root
/// (what `keys::normalize` makes paths relative to).
pub(super) struct Located {
    pub project_id: String,
    pub root: String,
    pub project_root: std::path::PathBuf,
}

type Fail = (StatusCode, String);

const NEEDS_SESSION_OR_RUN: &str = "task claims need a session or a run";

/// Resolves the worktree a request acts in, with the fence `verify` and `record` use
/// (`verify::resolve_worktree`): a run is held to its own worktree, the owner may name any
/// registered worktree of a known project. A refusal that says the project or the worktree is not
/// known becomes "claims unavailable", which the caller must read as "carry on, nothing is
/// claimed", not as a failure.
pub(super) async fn locate(
    pool: &sqlx::SqlitePool,
    scope: &Scope,
    worktree: Option<&str>,
) -> Result<Located, Fail> {
    let caller = crate::verify::Caller::from_scope(scope)
        .ok_or((StatusCode::FORBIDDEN, "this key cannot use task claims".to_owned()))?;
    if matches!(caller, crate::verify::Caller::Owner) && worktree.is_none() {
        return Err((StatusCode::FORBIDDEN, NEEDS_SESSION_OR_RUN.to_owned()));
    }
    let deadline = Instant::now() + crate::execution::git_exec::OPERATION_TIMEOUT;
    let (root, project_id) = crate::verify::resolve_worktree(pool, caller, worktree, deadline)
        .await
        .map_err(|error| match error {
            crate::verify::VerifyError::NotFound(why) => (
                StatusCode::NOT_FOUND,
                format!("task claims unavailable: project not registered ({why})"),
            ),
            other => (other.status(), other.message().to_owned()),
        })?;
    let root = crate::execution::git_exec::canonical(&root)
        .await
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, e))?;
    // `crate::inspect`, as `verify.rs` reads it; it answers `Option<String>`.
    let project_root = crate::inspect::project_root(pool, &project_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                "task claims unavailable: project not registered (no root recorded)".to_owned(),
            )
        })?;
    Ok(Located { project_id, root, project_root: project_root.into() })
}

fn internal(error: impl std::fmt::Display) -> Fail {
    (StatusCode::INTERNAL_SERVER_ERROR, format!("task claims: {error}"))
}

/// The shape the hook also checks (`SESSION_ID_SHAPE` in `ask_daemon.py`).
fn valid_session_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Who the caller is, as a claim holder. The run's identity comes from the key's scope, the
/// session's from the id the hook stamped, and only if `session_seen_in` has seen it in this very
/// worktree, which catches an id copied wrong or left over from another session.
pub(super) async fn holder_for(
    pool: &sqlx::SqlitePool,
    scope: &Scope,
    at: &Located,
    session_id: Option<&str>,
) -> Result<Holder, Fail> {
    match scope {
        Scope::Run(run_id) => {
            // A job node is a run whose row carries the job: the claim belongs to the job, which
            // outlives the node. A plain run holds by its own id.
            let job: Option<Option<i64>> = sqlx::query_scalar("SELECT job_id FROM runs WHERE id = ?")
                .bind(run_id)
                .fetch_optional(pool)
                .await
                .map_err(internal)?;
            let (kind, id) = match job.flatten() {
                Some(job_id) => (HolderKind::Job, job_id.to_string()),
                None => (HolderKind::Run, run_id.to_string()),
            };
            Ok(Holder { kind, id, worktree: at.root.clone() })
        }
        Scope::Control | Scope::ApiToken(ApiTokenLevel::Admin) => {
            let session = session_id
                .ok_or_else(|| (StatusCode::FORBIDDEN, NEEDS_SESSION_OR_RUN.to_owned()))?;
            if !valid_session_id(session) {
                return Err((StatusCode::BAD_REQUEST, "session_id has an unexpected shape".to_owned()));
            }
            let seen = sessions::session_seen_in(pool, session, &at.root).await.map_err(internal)?;
            if !seen {
                return Err((
                    StatusCode::FORBIDDEN,
                    format!(
                        "session {session} has not been seen in {}; the first tool call registers it, retry",
                        at.root
                    ),
                ));
            }
            Ok(Holder { kind: HolderKind::Session, id: session.to_owned(), worktree: at.root.clone() })
        }
        _ => Err((StatusCode::FORBIDDEN, "this key cannot use task claims".to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_claims::{Holder, HolderKind, Notice};
    use std::time::{Duration, Instant};

    fn holder(kind: HolderKind, id: &str, worktree: &str) -> Holder {
        Holder { kind, id: id.to_owned(), worktree: worktree.to_owned() }
    }

    #[test]
    fn the_cwd_cache_remembers_a_project_and_forgets_it_after_the_ttl() {
        let t0 = Instant::now();
        let mut cache = CwdCache::default();
        assert_eq!(cache.get("/w", t0), None, "a miss is None, not a cached refusal");
        cache.put("/w", Some(("alpha".into(), "/w".into())), t0);
        assert_eq!(
            cache.get("/w", t0 + Duration::from_secs(599)),
            Some(Some(("alpha".into(), "/w".into())))
        );
        assert_eq!(cache.get("/w", t0 + CwdCache::POSITIVE_TTL + Duration::from_secs(1)), None);
    }

    #[test]
    fn an_unregistered_cwd_is_cached_for_a_short_time_only() {
        let t0 = Instant::now();
        let mut cache = CwdCache::default();
        cache.put("/x", None, t0);
        assert_eq!(cache.get("/x", t0 + Duration::from_secs(30)), Some(None));
        assert_eq!(
            cache.get("/x", t0 + CwdCache::NEGATIVE_TTL + Duration::from_secs(1)),
            None,
            "registering the project must show up within a minute"
        );
    }

    #[test]
    fn the_cwd_cache_is_bounded() {
        let t0 = Instant::now();
        let mut cache = CwdCache::default();
        for n in 0..(CwdCache::MAX_ENTRIES + 5) {
            cache.put(&format!("/w{n}"), None, t0);
        }
        assert!(cache.len() <= CwdCache::MAX_ENTRIES);
    }

    #[test]
    fn a_holder_is_named_the_way_an_agent_can_act_on() {
        assert_eq!(holder_label(&holder(HolderKind::Run, "7", "/w")), "run #7");
        assert_eq!(holder_label(&holder(HolderKind::Job, "3", "/w")), "job #3");
        assert_eq!(
            holder_label(&holder(HolderKind::Postgate, "alpha", "/pg")),
            "the daemon (post-merge gate)"
        );
        assert_eq!(
            holder_label(&holder(HolderKind::Session, "s1", "/w2")),
            "another session in /w2"
        );
    }

    #[test]
    fn a_lost_claim_says_who_has_the_task_now() {
        let task = crate::task_claims::testing::row("test:a::b", 12);
        let text = render_notice(&Notice::ClaimLost {
            task,
            new_holder: holder(HolderKind::Run, "9", "/w"),
        });
        assert!(text.contains("#12") && text.contains("test:a::b"), "{text}");
        assert!(text.contains("run #9"), "{text}");
        assert!(text.contains("stop"), "an agent must be told to stop, not only informed: {text}");
    }

    #[test]
    fn a_closed_incident_tells_the_session_to_stop_and_rebase() {
        let task = crate::task_claims::testing::row("master-red", 4);
        let text = render_notice(&Notice::IncidentClosed { task, sha: Some("abcdef1234567".into()) });
        assert!(text.contains("#4") && text.contains("abcdef1"), "{text}");
        assert!(!text.contains("abcdef1234567"), "the sha is shortened to 7: {text}");
        assert!(text.contains("rebase") && text.contains("verify"), "{text}");
    }

    #[test]
    fn an_incident_line_names_the_red_sha_and_the_owner() {
        let mut task = crate::task_claims::testing::row("master-red", 4);
        task.detail = serde_json::json!({ "red_since_sha": "1234567890abcdef" });
        task.holder_kind = Some(HolderKind::Run);
        task.holder_id = Some("21".into());
        let line = render_incident(&task);
        assert_eq!(line, "#4 master-red since 1234567: being fixed by run #21");

        task.holder_kind = None;
        task.holder_id = None;
        assert_eq!(render_incident(&task), "#4 master-red since 1234567: unclaimed");
    }

    fn located(project: &str, root: &str) -> Located {
        Located { project_id: project.into(), root: root.into(), project_root: root.into() }
    }

    /// The smallest `jobs` row (the columns `consolidate.rs`'s tests insert): `runs.job_id` is a
    /// foreign key to it.
    async fn insert_job(pool: &sqlx::SqlitePool) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO jobs (project_id, project_root, status, max_items, created_at)
             VALUES ('alpha', '/r', 'running', 3, '2026-10-22T00:00:00+00:00') RETURNING id",
        )
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn insert_run(pool: &sqlx::SqlitePool, cwd: &str, job: Option<i64>) -> i64 {
        sqlx::query(
            "INSERT INTO runs (prompt, status, mode, project_id, cwd, job_id, created_at)
             VALUES ('x', 'running', 'worktree', 'alpha', ?, ?, '2026-10-22T00:00:00Z')",
        )
        .bind(cwd)
        .bind(job)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    #[tokio::test]
    async fn a_plain_run_holds_by_its_run_id() {
        let pool = crate::testdb::fresh_pool().await;
        let run = insert_run(&pool, "/w", None).await;
        let holder = holder_for(&pool, &Scope::Run(run), &located("alpha", "/w"), None).await.unwrap();
        assert_eq!(holder, Holder { kind: HolderKind::Run, id: run.to_string(), worktree: "/w".into() });
    }

    #[tokio::test]
    async fn a_job_node_holds_by_its_job() {
        let pool = crate::testdb::fresh_pool().await;
        let job = insert_job(&pool).await;
        let run = insert_run(&pool, "/w", Some(job)).await;
        let holder = holder_for(&pool, &Scope::Run(run), &located("alpha", "/w"), None).await.unwrap();
        assert_eq!((holder.kind, holder.id.as_str()), (HolderKind::Job, job.to_string().as_str()));
    }

    #[tokio::test]
    async fn a_session_is_only_believed_where_it_was_seen() {
        let pool = crate::testdb::fresh_pool().await;
        let now = chrono::Utc::now();
        sessions::touch_session(&pool, "s1", "/w", "alpha", now).await.unwrap();

        let ok = holder_for(&pool, &Scope::Control, &located("alpha", "/w"), Some("s1")).await.unwrap();
        assert_eq!((ok.kind, ok.id.as_str(), ok.worktree.as_str()), (HolderKind::Session, "s1", "/w"));

        let elsewhere = holder_for(&pool, &Scope::Control, &located("alpha", "/other"), Some("s1")).await;
        assert_eq!(elsewhere.unwrap_err().0, StatusCode::FORBIDDEN, "seen in /w, not in /other");
        let unknown = holder_for(&pool, &Scope::Control, &located("alpha", "/w"), Some("nope")).await;
        assert_eq!(unknown.unwrap_err().0, StatusCode::FORBIDDEN);
        let missing = holder_for(&pool, &Scope::Control, &located("alpha", "/w"), None).await;
        assert_eq!(missing.unwrap_err().0, StatusCode::FORBIDDEN);
        let admin = holder_for(
            &pool,
            &Scope::ApiToken(ApiTokenLevel::Admin),
            &located("alpha", "/w"),
            Some("s1"),
        )
        .await;
        assert!(admin.is_ok());
        let shaped = holder_for(&pool, &Scope::Control, &located("alpha", "/w"), Some("../etc")).await;
        assert_eq!(shaped.unwrap_err().0, StatusCode::BAD_REQUEST, "the id shape is checked first");
    }

    #[tokio::test]
    async fn other_keys_hold_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        for scope in [Scope::ApiToken(ApiTokenLevel::ReadOnly), Scope::TeamRun("t".into())] {
            let refused = holder_for(&pool, &scope, &located("alpha", "/w"), Some("s1")).await;
            assert_eq!(refused.unwrap_err().0, StatusCode::FORBIDDEN);
        }
    }

    #[tokio::test]
    async fn a_checkout_outside_the_roster_answers_claims_unavailable() {
        let pool = crate::testdb::fresh_pool().await;
        let dir = tempfile::tempdir().unwrap();
        let init = std::process::Command::new("git")
            .args(["init", "-q", "-b", "main"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(init.success());
        let cwd = dir.path().to_string_lossy().into_owned();
        let refused = locate(&pool, &Scope::Control, Some(&cwd)).await.err().expect("not on the roster");
        assert_eq!(refused.0, StatusCode::NOT_FOUND);
        assert!(refused.1.starts_with("task claims unavailable: project not registered"), "{}", refused.1);
    }
}
