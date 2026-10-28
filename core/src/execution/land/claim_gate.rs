//! The land gate of the task claims (spec 2026-10-22 task-claims §3 Land).
//!
//! A branch whose changed files cross the `guard_paths` of a live, claimed task is refused unless
//! it comes from that task's owner. `hint_paths` appear in the message and never block. The gate
//! is fail-open: if git or the claims store cannot answer, the landing goes ahead, because a
//! database glitch must not stop the project's work.

use std::path::Path;
use std::time::Instant;

use chrono::{DateTime, Utc};

use crate::execution::git_exec;
use crate::execution::vcs::{Branch, Op, Origin};
use crate::task_claims::{self, Holder, HolderKind, TaskRow};

/// Who is landing what, as far as the gate cares.
pub struct Landing<'a> {
    pub project_id: &'a str,
    pub project_root: &'a Path,
    pub source: &'a Branch,
    pub target: &'a Branch,
    /// A conflict resolver's output, or the daemon acting on the owner's standing order.
    pub exempt: bool,
    /// The run that asked (`vcs_requests.run_id`), when one did.
    pub run_id: Option<i64>,
}

pub enum Verdict {
    Clear,
    Refused(String),
    /// The owner's override was recorded against these tasks.
    Overridden(Vec<i64>),
}

/// A guarded task, reduced to what the decision reads. Pure data, so the rule is testable without
/// building a `TaskRow`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Guard {
    pub id: i64,
    pub key: String,
    pub holder_label: String,
    pub holder_kind: Option<HolderKind>,
    pub holder_id: String,
    pub holder_worktree: String,
    pub guard_paths: Vec<String>,
    pub hint_paths: Vec<String>,
}

impl Guard {
    /// The holder the task records, for the `override_land` event; `None` if the row has no whole one.
    fn holder(&self) -> Option<Holder> {
        Some(Holder {
            kind: self.holder_kind?,
            id: self.holder_id.clone(),
            worktree: self.holder_worktree.clone(),
        })
    }
}

impl From<&TaskRow> for Guard {
    fn from(row: &TaskRow) -> Self {
        let id = row.holder_id.clone().unwrap_or_default();
        Guard {
            id: row.id,
            key: row.key.as_str().to_owned(),
            holder_label: format!(
                "{} {id}",
                row.holder_kind
                    .map(|k| format!("{k:?}").to_lowercase())
                    .unwrap_or_default()
            ),
            holder_kind: row.holder_kind,
            holder_id: id,
            holder_worktree: row.holder_worktree.clone().unwrap_or_default(),
            guard_paths: row.guard_paths.clone(),
            hint_paths: row.hint_paths.clone(),
        }
    }
}

/// Paths compared as the OS would: slashes folded, trailing slash dropped, case folded on Windows.
fn same_path(a: &str, b: &str) -> bool {
    let fold = |p: &str| {
        let p = p.replace('\\', "/");
        let p = p.trim_end_matches('/');
        if cfg!(windows) {
            p.to_lowercase()
        } else {
            p.to_owned()
        }
    };
    !a.is_empty() && fold(a) == fold(b)
}

/// The guards `landing` is not entitled to cross: the owner passes. The owner is the worktree that
/// holds the branch when it is exactly the task's `holder_worktree` (spec §3 Land: "cujo
/// `holder_worktree` não é a worktree do branch"), the run that owns the task, or a node of the job
/// that owns it (`job_id` is the asking run's `runs.job_id`).
///
/// Exact match only: every daemon worktree and every `.claude/worktrees/<name>` lives beneath the
/// main checkout, so "the holder's worktree lies beneath the branch's" would make a branch checked
/// out in the main checkout the owner of every claim. The writers store the canonical toplevel.
pub fn blocking<'a>(
    guards: &'a [Guard],
    owner_worktree: Option<&str>,
    run_id: Option<i64>,
    job_id: Option<i64>,
) -> Vec<&'a Guard> {
    guards
        .iter()
        .filter(|guard| {
            let by_worktree = owner_worktree.is_some_and(|w| same_path(w, &guard.holder_worktree));
            let by_run = matches!(guard.holder_kind, Some(HolderKind::Run))
                && run_id.is_some_and(|id| guard.holder_id == id.to_string());
            let by_job = matches!(guard.holder_kind, Some(HolderKind::Job))
                && job_id.is_some_and(|id| guard.holder_id == id.to_string());
            !(by_worktree || by_run || by_job)
        })
        .collect()
}

/// The job the run belongs to (`runs.job_id`), as `holder_for` (`http/tasks.rs`) reads it when a
/// job node claims: such a claim is held by the job, not by the node's run.
async fn job_of_run(pool: &sqlx::SqlitePool, run_id: i64) -> Result<Option<i64>, sqlx::Error> {
    let job: Option<Option<i64>> = sqlx::query_scalar("SELECT job_id FROM runs WHERE id = ?")
        .bind(run_id)
        .fetch_optional(pool)
        .await?;
    Ok(job.flatten())
}

/// The refusal text. It never mentions the owner's override: an agent reading this must not learn
/// a way round (see `NUCLEOS_ALLOW_CLAIMED_LAND`, Task 11).
pub fn refusal_message(blocking: &[&Guard]) -> String {
    let mut parts = Vec::new();
    for guard in blocking {
        let mut part = format!(
            "task #{} ({}) is held by {} and guards {}",
            guard.id,
            guard.key,
            guard.holder_label,
            guard.guard_paths.join(", ")
        );
        if !guard.hint_paths.is_empty() {
            part.push_str(&format!(
                " (also touched by it, never blocking: {})",
                guard.hint_paths.join(", ")
            ));
        }
        parts.push(part);
    }
    format!(
        "this branch changes files another session is fixing: {}. Wait for the task to close, then rebase and land again.",
        parts.join("; ")
    )
}

/// The `note` an override is recorded under, and looked up by at execution time.
fn override_note(source: &Branch, target: &Branch) -> String {
    format!("land {} -> {}", source.as_str(), target.as_str())
}

pub async fn check(
    pool: &sqlx::SqlitePool,
    landing: &Landing<'_>,
    owner_override: bool,
    deadline: Instant,
    now: DateTime<Utc>,
) -> Verdict {
    if landing.exempt {
        return Verdict::Clear;
    }
    let changed = match git_exec::changed_since_merge_base(
        landing.project_root,
        landing.target.as_str(),
        landing.source.as_str(),
        deadline,
    )
    .await
    {
        Ok(changed) if !changed.is_empty() => changed,
        Ok(_) => return Verdict::Clear,
        Err(reason) => {
            tracing::warn!(project = landing.project_id, %reason, "land: claims gate cannot read the changed files");
            return Verdict::Clear;
        }
    };
    let rows = match task_claims::store::guarding(pool, landing.project_id, &changed, now).await {
        Ok(rows) if !rows.is_empty() => rows,
        Ok(_) => return Verdict::Clear,
        Err(error) => {
            tracing::warn!(project = landing.project_id, %error, "land: claims gate cannot read the claims");
            return Verdict::Clear;
        }
    };
    // Fail-open like the rest of the gate: if git cannot say which worktree holds the branch (or
    // where it is), the owner cannot be told from a stranger, and a stranger is a refusal. A gate
    // that cannot read must not refuse, so the landing goes ahead. "No worktree holds it" is an
    // answer (`Ok(None)`), and then nobody owns the branch.
    let owner = match git_exec::worktree_of_branch(
        landing.project_root,
        landing.source.as_str(),
        deadline,
    )
    .await
    {
        Ok(Some(path)) => match git_exec::canonical(&path).await {
            Ok(canonical) => Some(canonical),
            Err(reason) => {
                tracing::warn!(project = landing.project_id, %reason, "land: claims gate cannot resolve the owner's worktree");
                return Verdict::Clear;
            }
        },
        Ok(None) => None,
        Err(reason) => {
            tracing::warn!(project = landing.project_id, %reason, "land: claims gate cannot find the branch's worktree");
            return Verdict::Clear;
        }
    };
    let guards: Vec<Guard> = rows.iter().map(Guard::from).collect();
    // A job node's claim is held by its job, so a landing from any node of that job is the owner's.
    // Read only when a job holds one of the guards. Fail-open like the owner lookup above: a read
    // error leaves the owner unknown, and the landing goes ahead.
    let job_id = match landing.run_id {
        Some(run_id)
            if guards
                .iter()
                .any(|g| g.holder_kind == Some(HolderKind::Job)) =>
        {
            match job_of_run(pool, run_id).await {
                Ok(job) => job,
                Err(error) => {
                    tracing::warn!(
                        project = landing.project_id,
                        %error,
                        "land: claims gate cannot read the run's job"
                    );
                    return Verdict::Clear;
                }
            }
        }
        _ => None,
    };
    let note = override_note(landing.source, landing.target);
    let mut still = Vec::new();
    for guard in blocking(&guards, owner.as_deref(), landing.run_id, job_id) {
        // An override recorded when this branch was admitted holds for the claim in force
        // (`override_recorded` is true only for an `override_land` event after the current
        // `claimed_at`, so a new owner's claim is not covered by an old override). A read error
        // means "not recorded": the landing is refused, the safe side.
        if task_claims::store::override_recorded(pool, guard.id, &note)
            .await
            .unwrap_or(false)
        {
            continue;
        }
        still.push(guard);
    }
    if still.is_empty() {
        return Verdict::Clear;
    }
    if owner_override {
        for guard in &still {
            if let Err(error) = task_claims::store::record_event(
                pool,
                guard.id,
                "override_land",
                guard.holder().as_ref(),
                &note,
                now,
            )
            .await
            {
                return Verdict::Refused(format!(
                    "the owner's override could not be recorded: {error}"
                ));
            }
        }
        tracing::warn!(project = landing.project_id, tasks = ?still.iter().map(|g| g.id).collect::<Vec<_>>(), "land: the owner overrode a claimed task");
        return Verdict::Overridden(still.iter().map(|g| g.id).collect());
    }
    Verdict::Refused(refusal_message(&still))
}

/// The queue-time re-check (spec §3 Land: "repetido quando a fila executa o merge"). Only an
/// `Op::Merge` is a landing: the postgate's `Op::Revert` is never gated, which is how its landing
/// from the `postgate_worktree` passes while the gate holds the incident that guards those files.
///
/// **Only a merge INTO the integration branch is a landing.** `drain_once` has already resolved it
/// for the claimed request (`ClaimedRequest.integration_branch`, `drain.rs:214-232`) and passes it
/// in as `integration`. A merge into any other branch (an agent merging `master` into its own
/// feature branch to catch up) brings the guarded files to nobody's mainline and is left alone, and
/// so is every merge when the integration branch could not be resolved (`None`: fail-open, as the
/// admission gate is). The reason is `Some` when the merge must not run.
// One call site (`drain_once`) passes these straight through; a struct for them would be churn.
#[allow(clippy::too_many_arguments)]
pub async fn refusal_for_op(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    project_root: &Path,
    op: &Op,
    from_resolution: bool,
    origin: Origin,
    run_id: Option<i64>,
    integration: Option<&Branch>,
) -> Option<String> {
    let Op::Merge { source, target } = op else {
        return None;
    };
    if integration.is_none_or(|branch| branch.as_str() != target.as_str()) {
        return None;
    }
    let landing = Landing {
        project_id,
        project_root,
        source,
        target,
        exempt: from_resolution || matches!(origin, Origin::Daemon),
        run_id,
    };
    let deadline = Instant::now() + git_exec::OPERATION_TIMEOUT;
    match check(pool, &landing, false, deadline, Utc::now()).await {
        Verdict::Refused(message) => {
            Some(format!("refused when the queue came to run it: {message}"))
        }
        _ => None,
    }
}

#[cfg(test)]
mod pure_tests {
    use super::*;

    fn guard(id: i64, kind: HolderKind, holder_id: &str, worktree: &str) -> Guard {
        Guard {
            id,
            key: format!("file:f{id}.rs"),
            holder_label: format!("{} {holder_id}", format!("{kind:?}").to_lowercase()),
            holder_kind: Some(kind),
            holder_id: holder_id.to_owned(),
            holder_worktree: worktree.to_owned(),
            guard_paths: vec![format!("f{id}.rs")],
            hint_paths: vec!["culprit.rs".to_owned()],
        }
    }

    #[test]
    fn the_owner_is_recognised_by_worktree_whatever_the_slashes_or_case() {
        let guards = [guard(1, HolderKind::Session, "s1", "C:/Work/Repo-wt")];
        // Slashes and a trailing separator never matter, on any platform.
        assert!(blocking(&guards, Some("C:\\Work\\Repo-wt\\"), None, None).is_empty());
        assert_eq!(
            blocking(&guards, Some("C:/Work/Other"), None, None).len(),
            1
        );
        assert_eq!(
            blocking(&guards, None, None, None).len(),
            1,
            "no worktree, no ownership"
        );
        // Case folds only where the filesystem does (`same_path`): on Linux `Repo-wt` and
        // `repo-wt` are two directories.
        #[cfg(windows)]
        assert!(blocking(&guards, Some("c:\\work\\repo-wt\\"), None, None).is_empty());
        #[cfg(not(windows))]
        assert_eq!(
            blocking(&guards, Some("c:/work/repo-wt"), None, None).len(),
            1
        );
    }

    #[test]
    fn a_branch_in_the_main_checkout_does_not_own_a_claim_held_in_a_worktree_beneath_it() {
        // Every daemon worktree and every `.claude/worktrees/<name>` lives beneath the main
        // checkout: a branch checked out there is a stranger to their claims.
        let nested = [
            guard(1, HolderKind::Session, "s1", "C:/Repo/.claude/worktrees/a"),
            guard(2, HolderKind::Run, "9", "C:/Repo/.nucleos/worktrees/run-9"),
        ];
        assert_eq!(blocking(&nested, Some("C:\\Repo\\"), None, None).len(), 2);
        // A sibling that merely shares the prefix is a stranger.
        let sibling = [guard(3, HolderKind::Session, "s1", "C:/Work/Repo-wt2")];
        assert_eq!(
            blocking(&sibling, Some("C:/Work/Repo-wt"), None, None).len(),
            1
        );
        // Nor does the claim on a parent of the branch's worktree make it the owner.
        let parent = [guard(4, HolderKind::Session, "s1", "C:/Work")];
        assert_eq!(
            blocking(&parent, Some("C:/Work/Repo-wt"), None, None).len(),
            1
        );
    }

    #[test]
    fn the_run_that_owns_the_task_passes_by_its_run_id() {
        let guards = [guard(2, HolderKind::Run, "7", "C:/gone")];
        assert!(blocking(&guards, None, Some(7), None).is_empty());
        assert_eq!(blocking(&guards, None, Some(8), None).len(), 1);
    }

    #[test]
    fn a_node_of_the_job_that_owns_the_task_passes_by_its_job_id() {
        let guards = [guard(5, HolderKind::Job, "3", "C:/gone")];
        // Another node (run 11) of job 3 lands: the job's claim is its own.
        assert!(blocking(&guards, None, Some(11), Some(3)).is_empty());
        assert_eq!(blocking(&guards, None, Some(11), Some(4)).len(), 1);
        assert_eq!(blocking(&guards, None, Some(11), None).len(), 1);
        // A run whose id happens to equal the job's id is not a node of it.
        assert_eq!(blocking(&guards, None, Some(3), None).len(), 1);
    }

    #[tokio::test]
    async fn a_runs_job_is_read_from_its_row() {
        let pool = crate::testdb::fresh_pool().await;
        let job: i64 = sqlx::query_scalar(
            "INSERT INTO jobs (project_id, project_root, status, max_items, created_at)
             VALUES ('alpha', '/r', 'running', 3, '2026-10-22T00:00:00+00:00') RETURNING id",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        let insert_run = |job: Option<i64>| {
            let pool = pool.clone();
            async move {
                sqlx::query(
                    "INSERT INTO runs (prompt, status, mode, project_id, cwd, job_id, created_at)
                     VALUES ('x', 'running', 'worktree', 'alpha', '/r', ?, \
                     '2026-10-22T00:00:00Z')",
                )
                .bind(job)
                .execute(&pool)
                .await
                .unwrap()
                .last_insert_rowid()
            }
        };
        let node = insert_run(Some(job)).await;
        let plain = insert_run(None).await;
        assert_eq!(job_of_run(&pool, node).await.unwrap(), Some(job));
        assert_eq!(job_of_run(&pool, plain).await.unwrap(), None);
        assert_eq!(job_of_run(&pool, plain + 100).await.unwrap(), None);
    }

    #[test]
    fn the_message_names_the_task_the_holder_and_the_hints_but_never_the_override() {
        let guards = [guard(12, HolderKind::Session, "s1", "C:/x")];
        let refs: Vec<&Guard> = guards.iter().collect();
        let message = refusal_message(&refs);
        assert!(
            message.contains("#12") && message.contains("file:f12.rs"),
            "{message}"
        );
        assert!(message.contains("session s1"), "{message}");
        assert!(
            message.contains("culprit.rs"),
            "hints are shown, not enforced: {message}"
        );
        assert!(
            !message.to_uppercase().contains("NUCLEOS_ALLOW"),
            "{message}"
        );
    }
}
