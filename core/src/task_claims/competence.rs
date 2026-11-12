//! Who is competent to fix a ticket (spec 2026-11-06-tickets-globais): the live holders whose work
//! touches the ticket's paths, and the context an agent reads beside a ticket.
//!
//! Three sources, in priority order. `Owned`: a `running` work node whose `owned_paths` overlap.
//! `Declared`: a live job whose `job_nodes.files` name a path. `Observed`: a live run or job whose
//! worktree touched a path (`worktree_touched_paths`), or a live interactive session whose branch
//! changed one since it parted from the ticket's target (git, bounded). Each loader is one SQL read
//! and returns an unordered list; [`rank`] is pure and does all the ordering.
//!
//! A `migration:` ticket is the exception: the land gate recorded the branch owner in `detail`
//! (`run_id`, `worktree`), and that owner is the one candidate.

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration as StdDuration, Instant};

use chrono::{DateTime, Duration, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{SqliteConnection, SqlitePool};

use super::store::{job_is_live, paths_overlap, run_is_live};
use super::{Holder, HolderKind, SESSION_LEASE_SECONDS, TaskRow};
use crate::execution::git_exec;
use crate::execution::land::IntegrationBranch;
use crate::execution::wave::stamp;
use crate::work_nodes::sets_overlap;

/// At most this many live interactive sessions are asked git about, newest first.
const OBSERVED_SESSIONS: i64 = 10;
/// The whole git budget of the observed-session source, for every session together.
const OBSERVED_DEADLINE: StdDuration = StdDuration::from_secs(5);
const MIGRATION_PREFIX: &str = "migration:";

/// Where a candidate's claim to competence comes from. The declaration order is the priority:
/// `Owned` beats `Declared` beats `Observed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Owned,
    Declared,
    Observed,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Candidate {
    pub holder: Holder,
    pub source: Source,
    /// A short line on what the holder is doing that touches the ticket.
    pub doing: String,
    /// When the holder was last seen active; compared as text, newest wins a tie.
    pub active_at: String,
}

/// Whether `a` is a better entry than `b` for the same holder: a stronger source, else newer.
fn better(a: &Candidate, b: &Candidate) -> bool {
    a.source < b.source || (a.source == b.source && a.active_at > b.active_at)
}

/// **Pure.** Drops the declined holders, keeps one entry per `(kind, id)` (its best source), and
/// orders by source priority, ties broken by the most recent `active_at`.
pub fn rank(found: Vec<Candidate>, declined: &HashSet<(HolderKind, String)>) -> Vec<Candidate> {
    let mut kept: Vec<Candidate> = Vec::new();
    for candidate in found {
        if declined.contains(&(candidate.holder.kind, candidate.holder.id.clone())) {
            continue;
        }
        match kept
            .iter_mut()
            .find(|entry| entry.holder.same_owner(&candidate.holder))
        {
            Some(entry) => {
                if better(&candidate, entry) {
                    *entry = candidate;
                }
            }
            None => kept.push(candidate),
        }
    }
    kept.sort_by(|a, b| {
        a.source
            .cmp(&b.source)
            .then_with(|| b.active_at.cmp(&a.active_at))
    });
    kept
}

/// The ticket's paths: guard paths and hint paths together.
fn ticket_paths(task: &TaskRow) -> Vec<String> {
    task.guard_paths
        .iter()
        .chain(&task.hint_paths)
        .cloned()
        .collect()
}

/// The first of `files` that is a ticket path, lies below one, or contains one.
fn first_touch<'a>(ticket: &[String], files: &'a [String]) -> Option<&'a String> {
    files.iter().find(|file| {
        ticket
            .iter()
            .any(|path| paths_overlap(path, file) || paths_overlap(file, path))
    })
}

fn decode_list(text: &str) -> Option<Vec<String>> {
    serde_json::from_str(text).ok()
}

/// The oldest `last_seen` a live session can have at `now`.
fn lease_cutoff(now: DateTime<Utc>) -> String {
    stamp(now - Duration::seconds(SESSION_LEASE_SECONDS))
}

/// Paths compared as the OS would: the `\\?\` prefix dropped, slashes folded, trailing slash dropped,
/// case folded on Windows. `detail.worktree` comes from git and `task_sessions.worktree` from a
/// canonicalisation, so the two spellings can differ for one directory.
fn same_path(a: &str, b: &str) -> bool {
    let fold = |p: &str| {
        let p = p.strip_prefix(r"\\?\").unwrap_or(p).replace('\\', "/");
        let p = p.trim_end_matches('/');
        if cfg!(windows) {
            p.to_lowercase()
        } else {
            p.to_owned()
        }
    };
    !a.is_empty() && fold(a) == fold(b)
}

/// The git answers of one request, shared by every ticket it builds a context for. One deadline
/// bounds all of the request's git work together ([`OBSERVED_DEADLINE`] in total, not per ticket),
/// and each `(target, worktree)` pair is asked git once.
#[derive(Debug, Default)]
pub struct SessionDiffs {
    /// Set on first git use, then reused by every later call of the request.
    deadline: Option<Instant>,
    /// `(target, worktree)` to `(branch, changed files)`; `None` skips the session (a git error, a
    /// detached HEAD, or a worktree on the target itself).
    memo: HashMap<(String, String), Option<SessionDiff>>,
}

/// A session's branch and the files it changed against the target.
type SessionDiff = (String, Vec<String>);

impl SessionDiffs {
    fn deadline(&mut self) -> Instant {
        *self
            .deadline
            .get_or_insert_with(|| Instant::now() + OBSERVED_DEADLINE)
    }

    /// How many `(target, worktree)` pairs were asked git.
    #[cfg(test)]
    pub(crate) fn memo_len(&self) -> usize {
        self.memo.len()
    }
}

/// Every live holder whose work touches the ticket, unranked (see [`rank`]). A ticket without paths
/// has none; a `migration:` ticket has at most its branch owner.
pub async fn candidates(
    pool: &SqlitePool,
    task: &TaskRow,
    now: DateTime<Utc>,
) -> sqlx::Result<Vec<Candidate>> {
    candidates_with(pool, task, now, &mut SessionDiffs::default()).await
}

/// [`candidates`] with the request's shared [`SessionDiffs`].
pub async fn candidates_with(
    pool: &SqlitePool,
    task: &TaskRow,
    now: DateTime<Utc>,
    diffs: &mut SessionDiffs,
) -> sqlx::Result<Vec<Candidate>> {
    if task.key.as_str().starts_with(MIGRATION_PREFIX) {
        return Ok(migration_owner(pool, task, now)
            .await?
            .into_iter()
            .collect());
    }
    let paths = ticket_paths(task);
    if paths.is_empty() {
        return Ok(Vec::new());
    }
    let mut found = {
        let mut conn = pool.acquire().await?;
        let mut found = owned(&mut conn, task, &paths, now).await?;
        found.extend(declared(&mut conn, task, &paths).await?);
        found.extend(observed_daemon(&mut conn, task, &paths).await?);
        found
    };
    found.extend(observed_sessions(pool, task, &paths, now, diffs).await?);
    Ok(found)
}

#[derive(sqlx::FromRow)]
struct NodeRow {
    title: String,
    owned_paths: String,
    run_id: Option<String>,
    session_id: Option<String>,
    updated_at: String,
    session_worktree: Option<String>,
    session_seen: Option<String>,
    run_worktree: Option<String>,
}

/// `Owned`: the `running` work nodes of the project whose `owned_paths` overlap the ticket's paths.
/// The holder is the node's run when it has one (live by `run_is_live`), else its session (live by
/// lease).
async fn owned(
    conn: &mut SqliteConnection,
    task: &TaskRow,
    paths: &[String],
    now: DateTime<Utc>,
) -> sqlx::Result<Vec<Candidate>> {
    let nodes = sqlx::query_as::<_, NodeRow>(
        "SELECT n.title, n.owned_paths, n.run_id, n.session_id, n.updated_at,
                s.worktree AS session_worktree, s.last_seen AS session_seen, w.path AS run_worktree
         FROM work_nodes n
         LEFT JOIN task_sessions s ON s.session_id = n.session_id
         LEFT JOIN worktrees w ON w.owner_kind = 'run' AND w.owner_id = CAST(n.run_id AS INTEGER)
                              AND w.removed_at IS NULL
         WHERE n.status = 'running' AND n.project_id = ?",
    )
    .bind(&task.project_id)
    .fetch_all(&mut *conn)
    .await?;
    let cutoff = lease_cutoff(now);
    let mut found = Vec::new();
    for node in nodes {
        let Some(owned_paths) = decode_list(&node.owned_paths) else {
            continue;
        };
        if sets_overlap(&owned_paths, paths).is_none() {
            continue;
        }
        let run_id = node.run_id.filter(|id| !id.trim().is_empty());
        let holder = match (run_id, node.session_id) {
            (Some(run), _) => {
                if !run_is_live(conn, &run).await? {
                    continue;
                }
                Holder {
                    kind: HolderKind::Run,
                    id: run,
                    worktree: node.run_worktree.unwrap_or_default(),
                }
            }
            (None, Some(session)) => {
                let live = node
                    .session_seen
                    .as_deref()
                    .is_some_and(|seen| seen >= cutoff.as_str());
                if !live {
                    continue;
                }
                Holder {
                    kind: HolderKind::Session,
                    id: session,
                    worktree: node.session_worktree.unwrap_or_default(),
                }
            }
            (None, None) => continue,
        };
        found.push(Candidate {
            holder,
            source: Source::Owned,
            doing: node.title,
            active_at: node.updated_at,
        });
    }
    Ok(found)
}

#[derive(sqlx::FromRow)]
struct JobNodeRow {
    job_id: i64,
    description: String,
    files: String,
    created_at: String,
    worktree: Option<String>,
}

/// `Declared`: the nodes of the project's live jobs whose `files` name a ticket path. The holder is
/// the job, which outlives its nodes.
async fn declared(
    conn: &mut SqliteConnection,
    task: &TaskRow,
    paths: &[String],
) -> sqlx::Result<Vec<Candidate>> {
    let rows = sqlx::query_as::<_, JobNodeRow>(
        "SELECT n.job_id, n.description, n.files, j.created_at, w.path AS worktree
         FROM job_nodes n
         JOIN jobs j ON j.id = n.job_id
         LEFT JOIN worktrees w ON w.owner_kind = 'job' AND w.owner_id = j.id AND w.removed_at IS NULL
         WHERE j.project_id = ? AND n.files IS NOT NULL",
    )
    .bind(&task.project_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut live: HashMap<i64, bool> = HashMap::new();
    let mut found = Vec::new();
    for row in rows {
        let Some(files) = decode_list(&row.files) else {
            continue;
        };
        if first_touch(paths, &files).is_none() {
            continue;
        }
        let id = row.job_id.to_string();
        let alive = match live.get(&row.job_id).copied() {
            Some(alive) => alive,
            None => {
                let alive = job_is_live(conn, &id).await?;
                live.insert(row.job_id, alive);
                alive
            }
        };
        if !alive {
            continue;
        }
        found.push(Candidate {
            holder: Holder {
                kind: HolderKind::Job,
                id,
                worktree: row.worktree.unwrap_or_default(),
            },
            source: Source::Declared,
            doing: row.description,
            active_at: row.created_at,
        });
    }
    Ok(found)
}

#[derive(sqlx::FromRow)]
struct TouchedRow {
    owner_kind: String,
    owner_id: i64,
    paths: String,
    measured_at: String,
    worktree: Option<String>,
}

/// `Observed` for the daemon's own worktrees: `worktree_touched_paths` holds only run and job
/// worktrees, so it serves only run and job holders.
async fn observed_daemon(
    conn: &mut SqliteConnection,
    task: &TaskRow,
    paths: &[String],
) -> sqlx::Result<Vec<Candidate>> {
    let rows = sqlx::query_as::<_, TouchedRow>(
        "SELECT t.owner_kind, t.owner_id, t.paths, t.measured_at, w.path AS worktree
         FROM worktree_touched_paths t
         LEFT JOIN worktrees w ON w.owner_kind = t.owner_kind AND w.owner_id = t.owner_id
                              AND w.removed_at IS NULL
         WHERE t.project_id = ? AND t.owner_kind IN ('run', 'job')",
    )
    .bind(&task.project_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut found = Vec::new();
    for row in rows {
        // Malformed reads as absent, never as "touched nothing" (0063).
        let Some(touched) = decode_list(&row.paths) else {
            continue;
        };
        let Some(file) = first_touch(paths, &touched) else {
            continue;
        };
        let doing = format!("changed {file}");
        let id = row.owner_id.to_string();
        let (kind, alive) = match row.owner_kind.as_str() {
            "run" => (HolderKind::Run, run_is_live(conn, &id).await?),
            "job" => (HolderKind::Job, job_is_live(conn, &id).await?),
            _ => continue,
        };
        if !alive {
            continue;
        }
        found.push(Candidate {
            holder: Holder {
                kind,
                id,
                worktree: row.worktree.unwrap_or_default(),
            },
            source: Source::Observed,
            doing,
            active_at: row.measured_at,
        });
    }
    Ok(found)
}

/// The branch the ticket is about: `detail.target` when the opener recorded one, else the project's
/// integration branch as `land::integration_branch_reading` reports it (read-only: it never writes a
/// derived default back). `None` when there is no usable answer.
async fn target_of(pool: &SqlitePool, task: &TaskRow, deadline: Instant) -> Option<String> {
    if let Some(target) = task
        .detail
        .get("target")
        .and_then(Value::as_str)
        .filter(|target| !target.trim().is_empty())
    {
        return Some(target.to_owned());
    }
    match crate::execution::land::integration_branch_reading(pool, &task.project_id, deadline)
        .await
        .ok()?
    {
        IntegrationBranch::Declared { branch } | IntegrationBranch::Derived { branch } => {
            Some(branch)
        }
        _ => None,
    }
}

/// `Observed` for interactive sessions, which are never in `worktree_touched_paths`: for each live
/// session of the project (at most [`OBSERVED_SESSIONS`], newest first) the files its branch changed
/// since it parted from the ticket's target. Every git call of the request shares the deadline of
/// `diffs`, and a `(target, worktree)` already asked is answered from its memo; a git error drops
/// that session, never the whole answer.
async fn observed_sessions(
    pool: &SqlitePool,
    task: &TaskRow,
    paths: &[String],
    now: DateTime<Utc>,
    diffs: &mut SessionDiffs,
) -> sqlx::Result<Vec<Candidate>> {
    // `run-<id>` rows are the notice cursors `offers` registers for runs (`sessions::register_pseudo`),
    // not interactive sessions: the run is already a candidate as a `Run` holder, so it must not come
    // back as an observed `Session` too.
    let sessions: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT session_id, worktree, last_seen FROM task_sessions
         WHERE project_id = ? AND last_seen >= ? AND session_id NOT LIKE 'run-%'
         ORDER BY last_seen DESC LIMIT ?",
    )
    .bind(&task.project_id)
    .bind(lease_cutoff(now))
    .bind(OBSERVED_SESSIONS)
    .fetch_all(pool)
    .await?;
    if sessions.is_empty() {
        return Ok(Vec::new());
    }
    let Some(root) = crate::inspect::project_root(pool, &task.project_id).await? else {
        return Ok(Vec::new());
    };
    let root = git_exec::without_verbatim_prefix(Path::new(&root));
    let deadline = diffs.deadline();
    let Some(target) = target_of(pool, task, deadline).await else {
        return Ok(Vec::new());
    };
    let mut found = Vec::new();
    for (session_id, worktree, last_seen) in sessions {
        let answer = match diffs.memo.entry((target.clone(), worktree.clone())) {
            Entry::Occupied(asked) => asked.into_mut(),
            Entry::Vacant(unasked) => {
                unasked.insert(session_diff(&root, &target, &worktree, deadline).await)
            }
        };
        let Some((_, changed)) = &*answer else {
            continue;
        };
        let Some(file) = first_touch(paths, changed) else {
            continue;
        };
        found.push(Candidate {
            doing: format!("changed {file}"),
            holder: Holder {
                kind: HolderKind::Session,
                id: session_id,
                worktree,
            },
            source: Source::Observed,
            active_at: last_seen,
        });
    }
    Ok(found)
}

/// The branch checked out in `worktree` and the files it changed since it parted from `target`, or
/// `None` when the session has nothing to diff: a git error, a detached HEAD (no branch to diff), or
/// the target itself (it changed nothing of its own).
async fn session_diff(
    root: &Path,
    target: &str,
    worktree: &str,
    deadline: Instant,
) -> Option<(String, Vec<String>)> {
    let at = git_exec::without_verbatim_prefix(Path::new(worktree));
    let branch = git_exec::current_branch(&at, deadline).await.ok()?;
    if branch == "HEAD" || branch == target {
        return None;
    }
    let changed = git_exec::changed_since_merge_base(root, target, &branch, deadline)
        .await
        .ok()?;
    Some((branch, changed))
}

/// The owner the land gate recorded on a `migration:` ticket: the run of `detail.run_id` when it is
/// live, else the live session whose worktree is `detail.worktree`. Never a lookup by branch.
async fn migration_owner(
    pool: &SqlitePool,
    task: &TaskRow,
    now: DateTime<Utc>,
) -> sqlx::Result<Option<Candidate>> {
    let worktree = task
        .detail
        .get("worktree")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let doing = match task.detail.get("branch").and_then(Value::as_str) {
        Some(branch) => format!("owns branch {branch}"),
        None => "owns the colliding branch".to_owned(),
    };
    let run_id = match task.detail.get("run_id") {
        Some(Value::Number(number)) => number.as_i64().map(|id| id.to_string()),
        Some(Value::String(text)) if !text.trim().is_empty() => Some(text.clone()),
        _ => None,
    };
    if let Some(run) = run_id {
        let mut conn = pool.acquire().await?;
        if run_is_live(&mut conn, &run).await? {
            return Ok(Some(Candidate {
                holder: Holder {
                    kind: HolderKind::Run,
                    id: run,
                    worktree,
                },
                source: Source::Owned,
                doing,
                active_at: stamp(now),
            }));
        }
    }
    if worktree.is_empty() {
        return Ok(None);
    }
    let sessions: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT session_id, worktree, last_seen FROM task_sessions
         WHERE project_id = ? AND last_seen >= ? ORDER BY last_seen DESC",
    )
    .bind(&task.project_id)
    .bind(lease_cutoff(now))
    .fetch_all(pool)
    .await?;
    Ok(sessions
        .into_iter()
        .find(|(_, seen_in, _)| same_path(&worktree, seen_in))
        .map(|(session_id, seen_in, last_seen)| Candidate {
            holder: Holder {
                kind: HolderKind::Session,
                id: session_id,
                worktree: seen_in,
            },
            source: Source::Owned,
            doing,
            active_at: last_seen,
        }))
}

/// The holders that declined this ticket: `declined` events, and `ceded` events (a decliner passing
/// the ticket on). In both, `holder_*` is the decliner. They are not proposed again.
pub async fn declined_set(
    pool: &SqlitePool,
    task_id: i64,
) -> sqlx::Result<HashSet<(HolderKind, String)>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT holder_kind, holder_id FROM task_claim_events
         WHERE task_id = ? AND event IN ('declined', 'ceded')
           AND holder_kind IS NOT NULL AND holder_id IS NOT NULL",
    )
    .bind(task_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .filter_map(|(kind, id)| HolderKind::parse(&kind).map(|kind| (kind, id)))
        .collect())
}

#[derive(sqlx::FromRow)]
struct ReportRow {
    reporter_kind: String,
    reporter_id: String,
    reporter_worktree: Option<String>,
    at: String,
    detail: String,
    paths: String,
    tests: String,
}

/// What an agent reads beside a ticket: every stored report, the ranked competent holders and the
/// sha that introduced the problem when the opener knew it.
pub async fn context(pool: &SqlitePool, task: &TaskRow, now: DateTime<Utc>) -> sqlx::Result<Value> {
    context_with(pool, task, now, &mut SessionDiffs::default()).await
}

/// [`context`] with the request's shared [`SessionDiffs`]: a listing builds every context through
/// one, so its git work is bounded once, not once per ticket.
pub async fn context_with(
    pool: &SqlitePool,
    task: &TaskRow,
    now: DateTime<Utc>,
    diffs: &mut SessionDiffs,
) -> sqlx::Result<Value> {
    let rows = sqlx::query_as::<_, ReportRow>(
        "SELECT reporter_kind, reporter_id, reporter_worktree, at, detail, paths, tests
         FROM task_claim_reports WHERE task_id = ? ORDER BY id",
    )
    .bind(task.id)
    .fetch_all(pool)
    .await?;
    let reports: Vec<Value> = rows
        .into_iter()
        .map(|row| {
            json!({
                "who": {
                    "kind": row.reporter_kind,
                    "id": row.reporter_id,
                    "worktree": row.reporter_worktree,
                },
                "at": row.at,
                "detail": row.detail,
                "paths": decode_list(&row.paths).unwrap_or_default(),
                "tests": decode_list(&row.tests).unwrap_or_default(),
            })
        })
        .collect();
    let declined = declined_set(pool, task.id).await?;
    let competent: Vec<Value> = rank(candidates_with(pool, task, now, diffs).await?, &declined)
        .into_iter()
        .map(|candidate| {
            json!({
                "holder": candidate.holder,
                "source": candidate.source,
                "doing": candidate.doing,
            })
        })
        .collect();
    let introduced_by = ["introduced_by", "red_since_sha"]
        .iter()
        .find_map(|field| task.detail.get(*field).and_then(Value::as_str))
        .map(str::to_owned);
    Ok(json!({
        "reports": reports,
        "competent": competent,
        "introduced_by": introduced_by,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_claims::testing;
    use chrono::TimeZone;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 7, 10, 0, 0).unwrap()
    }

    fn holder(kind: HolderKind, id: &str) -> Holder {
        Holder {
            kind,
            id: id.to_owned(),
            worktree: format!("/wt/{id}"),
        }
    }

    fn candidate(kind: HolderKind, id: &str, source: Source, active_at: &str) -> Candidate {
        Candidate {
            holder: holder(kind, id),
            source,
            doing: format!("{id} via {source:?}"),
            active_at: active_at.to_owned(),
        }
    }

    fn ids(ranked: &[Candidate]) -> Vec<&str> {
        ranked.iter().map(|c| c.holder.id.as_str()).collect()
    }

    /// A ticket of project `alpha` guarding `paths`.
    fn ticket(key: &str, paths: &[&str]) -> TaskRow {
        let mut row = testing::row(key, 1);
        row.guard_paths = paths.iter().map(|p| (*p).to_owned()).collect();
        row
    }

    #[test]
    fn rank_prefers_owned_over_declared_over_observed() {
        let ranked = rank(
            vec![
                candidate(HolderKind::Session, "observed", Source::Observed, "3"),
                candidate(HolderKind::Job, "declared", Source::Declared, "2"),
                candidate(HolderKind::Run, "owned", Source::Owned, "1"),
            ],
            &HashSet::new(),
        );
        assert_eq!(ids(&ranked), vec!["owned", "declared", "observed"]);
    }

    #[test]
    fn rank_breaks_ties_by_the_most_recent_activity() {
        let ranked = rank(
            vec![
                candidate(
                    HolderKind::Run,
                    "older",
                    Source::Owned,
                    "2026-11-07T09:00:00Z",
                ),
                candidate(
                    HolderKind::Run,
                    "newer",
                    Source::Owned,
                    "2026-11-07T10:00:00Z",
                ),
            ],
            &HashSet::new(),
        );
        assert_eq!(ids(&ranked), vec!["newer", "older"]);
    }

    #[test]
    fn rank_drops_declined_holders() {
        let declined: HashSet<_> = [(HolderKind::Run, "7".to_owned())].into_iter().collect();
        let ranked = rank(
            vec![
                candidate(HolderKind::Run, "7", Source::Owned, "1"),
                candidate(HolderKind::Session, "7", Source::Observed, "1"),
            ],
            &declined,
        );
        assert_eq!(ranked.len(), 1, "{ranked:?}");
        assert_eq!(
            ranked[0].holder.kind,
            HolderKind::Session,
            "the same id under another kind is another holder"
        );
    }

    #[test]
    fn rank_keeps_one_entry_per_holder_with_its_best_source() {
        let ranked = rank(
            vec![
                candidate(HolderKind::Job, "3", Source::Observed, "9"),
                candidate(HolderKind::Job, "3", Source::Declared, "1"),
                candidate(HolderKind::Run, "8", Source::Observed, "5"),
            ],
            &HashSet::new(),
        );
        assert_eq!(ids(&ranked), vec!["3", "8"]);
        assert_eq!(ranked[0].source, Source::Declared);
    }

    async fn insert_run(pool: &SqlitePool, status: &str) -> i64 {
        sqlx::query("INSERT INTO runs (prompt, status, created_at) VALUES ('x', ?, 't')")
            .bind(status)
            .execute(pool)
            .await
            .unwrap()
            .last_insert_rowid()
    }

    async fn insert_job(pool: &SqlitePool, status: &str) -> i64 {
        sqlx::query(
            "INSERT INTO jobs (project_id, project_root, status, max_items, created_at)
             VALUES ('alpha', '/r', ?, 1, 't')",
        )
        .bind(status)
        .execute(pool)
        .await
        .unwrap()
        .last_insert_rowid()
    }

    async fn insert_job_node(pool: &SqlitePool, job_id: i64, files: &[&str]) {
        sqlx::query(
            "INSERT INTO job_nodes (job_id, node_id, description, status, round, files)
             VALUES (?, 'implement', 'edit a', 'running', 0, ?)",
        )
        .bind(job_id)
        .bind(serde_json::to_string(files).unwrap())
        .execute(pool)
        .await
        .unwrap();
    }

    /// A work node of project `alpha`.
    async fn insert_node(
        pool: &SqlitePool,
        id: &str,
        status: &str,
        owned: &[&str],
        run_id: Option<i64>,
        session_id: Option<&str>,
    ) {
        sqlx::query(
            "INSERT INTO work_nodes (id, project_id, root_id, depth, kind, title, goal, status,
                                     owned_paths, run_id, session_id, created_at, updated_at)
             VALUES (?, 'alpha', ?, 0, 'leaf', ?, 'goal', ?, ?, ?, ?, 't', 't')",
        )
        .bind(id)
        .bind(id)
        .bind(format!("node {id}"))
        .bind(status)
        .bind(serde_json::to_string(owned).unwrap())
        .bind(run_id.map(|id| id.to_string()))
        .bind(session_id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_session(pool: &SqlitePool, id: &str, worktree: &str, seen: DateTime<Utc>) {
        sqlx::query(
            "INSERT INTO task_sessions (session_id, project_id, worktree, last_seen)
             VALUES (?, 'alpha', ?, ?)",
        )
        .bind(id)
        .bind(worktree)
        .bind(stamp(seen))
        .execute(pool)
        .await
        .unwrap();
    }

    fn keys_of(found: &[Candidate]) -> Vec<(HolderKind, String, Source)> {
        found
            .iter()
            .map(|c| (c.holder.kind, c.holder.id.clone(), c.source))
            .collect()
    }

    #[tokio::test]
    async fn a_running_node_owning_an_overlapping_path_is_a_candidate_and_a_planned_one_is_not() {
        let pool = crate::testdb::fresh_pool().await;
        let running = insert_run(&pool, "running").await;
        let other = insert_run(&pool, "running").await;
        insert_node(
            &pool,
            "n1",
            "running",
            &["core/src/**"],
            Some(running),
            None,
        )
        .await;
        insert_node(&pool, "n2", "planned", &["core/src/**"], Some(other), None).await;
        insert_node(&pool, "n3", "running", &["shell/**"], Some(other), None).await;

        let found = candidates(
            &pool,
            &ticket("file:core/src/a.rs", &["core/src/a.rs"]),
            t0(),
        )
        .await
        .unwrap();
        assert_eq!(
            keys_of(&found),
            vec![(HolderKind::Run, running.to_string(), Source::Owned)]
        );
    }

    #[tokio::test]
    async fn a_session_whose_lease_lapsed_is_not_a_candidate() {
        let pool = crate::testdb::fresh_pool().await;
        let now = t0();
        insert_session(
            &pool,
            "stale",
            "/wt/stale",
            now - Duration::seconds(SESSION_LEASE_SECONDS + 60),
        )
        .await;
        insert_session(&pool, "fresh", "/wt/fresh", now - Duration::seconds(60)).await;
        insert_node(&pool, "n1", "running", &["core/a.rs"], None, Some("stale")).await;
        insert_node(&pool, "n2", "running", &["core/a.rs"], None, Some("fresh")).await;

        let found = candidates(&pool, &ticket("file:core/a.rs", &["core/a.rs"]), now)
            .await
            .unwrap();
        assert_eq!(
            keys_of(&found),
            vec![(HolderKind::Session, "fresh".to_owned(), Source::Owned)]
        );
        assert_eq!(found[0].holder.worktree, "/wt/fresh");
    }

    #[tokio::test]
    async fn a_live_job_declaring_the_file_is_a_declared_candidate() {
        let pool = crate::testdb::fresh_pool().await;
        let dead = insert_job(&pool, "done").await;
        insert_job_node(&pool, dead, &["core/a.rs"]).await;
        let live = insert_job(&pool, "running").await;
        insert_job_node(&pool, live, &["core/a.rs", "core/b.rs"]).await;

        let found = candidates(&pool, &ticket("file:core/a.rs", &["core/a.rs"]), t0())
            .await
            .unwrap();
        assert_eq!(
            keys_of(&found),
            vec![(HolderKind::Job, live.to_string(), Source::Declared)]
        );
    }

    #[tokio::test]
    async fn a_migration_ticket_has_only_the_branch_owner_as_candidate() {
        let pool = crate::testdb::fresh_pool().await;
        let owner = insert_run(&pool, "running").await;
        let bystander = insert_run(&pool, "running").await;
        // Owns the very path the ticket hints at: on any other ticket it would be a candidate.
        insert_node(
            &pool,
            "n1",
            "running",
            &["core/migrations/**"],
            Some(bystander),
            None,
        )
        .await;
        let mut row = testing::row("migration:master:0276", 1);
        row.hint_paths = vec!["core/migrations/0276_a.sql".to_owned()];
        row.detail = json!({ "branch": "feat", "run_id": owner, "worktree": "/wt/feat" });

        let found = candidates(&pool, &row, t0()).await.unwrap();
        assert_eq!(
            keys_of(&found),
            vec![(HolderKind::Run, owner.to_string(), Source::Owned)]
        );
        assert_eq!(found[0].holder.worktree, "/wt/feat");
    }

    #[tokio::test]
    async fn a_migration_ticket_owner_comes_from_the_recorded_run_or_worktree() {
        let pool = crate::testdb::fresh_pool().await;
        let now = t0();
        let dead = insert_run(&pool, "interrupted").await;
        let mut row = testing::row("migration:master:0276", 1);
        row.detail = json!({ "branch": "feat", "run_id": dead, "worktree": "/wt/feat" });
        insert_session(&pool, "elsewhere", "/wt/other", now).await;

        assert!(
            candidates(&pool, &row, now).await.unwrap().is_empty(),
            "a dead run and no live session in its worktree: nobody"
        );

        insert_session(
            &pool,
            "lapsed",
            "/wt/feat/",
            now - Duration::seconds(SESSION_LEASE_SECONDS + 1),
        )
        .await;
        assert!(
            candidates(&pool, &row, now).await.unwrap().is_empty(),
            "a session whose lease lapsed counts for nothing"
        );

        insert_session(&pool, "owner", "/wt/feat", now).await;
        let found = candidates(&pool, &row, now).await.unwrap();
        assert_eq!(
            keys_of(&found),
            vec![(HolderKind::Session, "owner".to_owned(), Source::Owned)]
        );
    }

    #[tokio::test]
    async fn a_ticket_without_paths_has_no_candidates() {
        let pool = crate::testdb::fresh_pool().await;
        let run = insert_run(&pool, "running").await;
        insert_node(&pool, "n1", "running", &["**"], Some(run), None).await;
        let job = insert_job(&pool, "running").await;
        insert_job_node(&pool, job, &["core/a.rs"]).await;

        let found = candidates(&pool, &ticket("topic:flaky", &[]), t0())
            .await
            .unwrap();
        assert!(found.is_empty(), "{found:?}");
    }

    fn git(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    /// Project `alpha` on a git repo whose `master` is the integration branch, plus a worktree on
    /// branch `feat` that committed `core/a.rs`. Returns the pool, the container (keep it alive),
    /// the repo and the worktree's canonical path.
    async fn observed_setup() -> (SqlitePool, tempfile::TempDir, std::path::PathBuf, String) {
        let pool = crate::testdb::fresh_pool().await;
        let container = git_exec::testkit::space_free_tempdir("nucleos-competence-");
        let repo = container.path().join("repo");
        git_exec::testkit::initialize_repo(&repo);
        git(&repo, &["branch", "-M", "master"]);
        let tree = container.path().join("wt");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "feat",
                tree.to_str().unwrap(),
            ],
        );
        std::fs::create_dir_all(tree.join("core")).unwrap();
        std::fs::write(tree.join("core/a.rs"), "fn a() {}\n").unwrap();
        git(&tree, &["add", "-A"]);
        git(&tree, &["commit", "-q", "-m", "a"]);
        sqlx::query(
            "INSERT INTO autopilot_state (project_id, mode, project_root, integration_branch)
             VALUES ('alpha', 'active', ?, 'master')",
        )
        .bind(repo.to_string_lossy().into_owned())
        .execute(&pool)
        .await
        .unwrap();
        let worktree = git_exec::canonical(&tree).await.unwrap();
        (pool, container, repo, worktree)
    }

    #[tokio::test]
    async fn one_listing_asks_git_once_per_session_however_many_tickets_it_shows() {
        let (pool, _container, _repo, worktree) = observed_setup().await;
        let now = Utc::now();
        insert_session(&pool, "s1", &worktree, now).await;
        let mut diffs = SessionDiffs::default();

        for (id, key) in [(1, "file:core/a.rs"), (2, "topic:a-fails")] {
            let mut task = ticket(key, &["core/a.rs"]);
            task.id = id;
            let context = context_with(&pool, &task, now, &mut diffs).await.unwrap();
            assert_eq!(
                context["competent"][0]["holder"]["id"], "s1",
                "{key}: {context}"
            );
            assert_eq!(
                context["competent"][0]["source"], "observed",
                "{key}: {context}"
            );
        }
        assert_eq!(
            diffs.memo_len(),
            1,
            "two tickets over one session ask git about it once"
        );
    }

    #[tokio::test]
    async fn rank_never_proposes_a_holder_that_ceded_the_ticket() {
        let pool = crate::testdb::fresh_pool().await;
        let now = Utc::now();
        let reporter = holder(HolderKind::Session, "reporter");
        let mut new = testing::new_task("file:core/a.rs", super::super::TaskKind::Declared);
        new.hint_paths = vec!["core/a.rs".to_owned()];
        let task_id = super::super::store::report(
            &pool,
            "alpha",
            new,
            &super::super::Reporter::Holder(&reporter),
            "a fails",
            &[],
            false,
            None,
            now,
        )
        .await
        .unwrap()
        .task
        .id;
        for (event, kind, id) in [
            ("declined", "run", "7"),
            ("ceded", "session", "s9"),
            ("claimed", "job", "3"),
        ] {
            sqlx::query(
                "INSERT INTO task_claim_events (task_id, event, holder_kind, holder_id, holder_worktree, at, note)
                 VALUES (?, ?, ?, ?, '/wt', 't', '')",
            )
            .bind(task_id)
            .bind(event)
            .bind(kind)
            .bind(id)
            .execute(&pool)
            .await
            .unwrap();
        }

        let declined = declined_set(&pool, task_id).await.unwrap();
        assert!(
            declined.contains(&(HolderKind::Run, "7".to_owned())),
            "{declined:?}"
        );
        assert!(
            declined.contains(&(HolderKind::Session, "s9".to_owned())),
            "a holder that ceded the ticket declined it: {declined:?}"
        );
        assert!(
            !declined.contains(&(HolderKind::Job, "3".to_owned())),
            "a claim is no decline: {declined:?}"
        );
        let ranked = rank(
            vec![
                candidate(HolderKind::Session, "s9", Source::Owned, "1"),
                candidate(HolderKind::Job, "3", Source::Declared, "1"),
            ],
            &declined,
        );
        assert_eq!(ids(&ranked), vec!["3"]);
    }

    #[tokio::test]
    async fn an_interactive_session_with_changes_in_its_worktree_is_an_observed_candidate_without_reading_the_touched_paths_table()
     {
        let (pool, _container, repo, worktree) = observed_setup().await;
        let now = Utc::now();
        insert_session(&pool, "s1", &worktree, now).await;
        // On master: its branch changed nothing of its own.
        insert_session(&pool, "s2", &repo.to_string_lossy(), now).await;
        let touched: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM worktree_touched_paths")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(
            touched, 0,
            "the candidate must come from git, not the table"
        );

        let found = candidates(&pool, &ticket("file:core/a.rs", &["core/a.rs"]), now)
            .await
            .unwrap();
        assert_eq!(
            keys_of(&found),
            vec![(HolderKind::Session, "s1".to_owned(), Source::Observed)]
        );
        assert_eq!(found[0].doing, "changed core/a.rs");

        let elsewhere = candidates(&pool, &ticket("file:shell/b.ts", &["shell/b.ts"]), now)
            .await
            .unwrap();
        assert!(elsewhere.is_empty(), "{elsewhere:?}");
    }

    #[tokio::test]
    async fn a_run_pseudo_session_is_not_an_observed_candidate() {
        let (pool, _container, _repo, worktree) = observed_setup().await;
        let now = Utc::now();
        // The cursor row `offers` gives a run it offered a ticket to, in a worktree whose branch
        // changed the ticket's file.
        super::super::sessions::register_pseudo(&pool, "run-7", "alpha", &worktree, 0, now)
            .await
            .unwrap();

        let found = candidates(&pool, &ticket("file:core/a.rs", &["core/a.rs"]), now)
            .await
            .unwrap();
        assert!(found.is_empty(), "{:?}", keys_of(&found));
    }

    #[tokio::test]
    async fn the_context_carries_every_report_the_competent_list_and_the_introducing_sha() {
        let pool = crate::testdb::fresh_pool().await;
        let now = Utc::now();
        let (first, second) = (
            holder(HolderKind::Session, "s1"),
            holder(HolderKind::Session, "s2"),
        );
        let mut new = testing::new_task("file:core/a.rs", super::super::TaskKind::Declared);
        new.hint_paths = vec!["core/a.rs".to_owned()];
        new.detail = json!({ "introduced_by": "abc1234" });
        let mut task_id = 0;
        for (who, text) in [(&first, "a fails"), (&second, "a fails here too")] {
            let outcome = super::super::store::report(
                &pool,
                "alpha",
                new.clone(),
                &super::super::Reporter::Holder(who),
                text,
                &["core::a".to_owned()],
                false,
                None,
                now,
            )
            .await
            .unwrap();
            task_id = outcome.task.id;
        }
        let run = insert_run(&pool, "running").await;
        insert_node(&pool, "n1", "running", &["core/**"], Some(run), None).await;
        let mut conn = pool.acquire().await.unwrap();
        let task = super::super::store::row_by_id(&mut conn, task_id)
            .await
            .unwrap()
            .unwrap();
        drop(conn);

        let context = context(&pool, &task, now).await.unwrap();
        let reports = context["reports"].as_array().unwrap();
        assert_eq!(reports.len(), 2, "{context}");
        assert_eq!(reports[0]["who"]["id"], "s1");
        assert_eq!(reports[1]["detail"], "a fails here too");
        assert_eq!(reports[1]["paths"], json!(["core/a.rs"]));
        assert_eq!(reports[1]["tests"], json!(["core::a"]));
        assert!(reports[0]["at"].is_string());
        assert_eq!(context["competent"][0]["holder"]["id"], run.to_string());
        assert_eq!(context["competent"][0]["source"], "owned");
        assert_eq!(context["competent"][0]["doing"], "node n1");
        assert_eq!(context["introduced_by"], "abc1234");

        let mut red = testing::row("master-red", 99);
        red.detail = json!({ "red_since_sha": "def5678" });
        assert_eq!(
            super::context(&pool, &red, now).await.unwrap()["introduced_by"],
            "def5678"
        );
        assert_eq!(
            super::context(&pool, &testing::row("topic:x", 98), now)
                .await
                .unwrap()["introduced_by"],
            Value::Null
        );
    }
}
