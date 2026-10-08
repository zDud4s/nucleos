//! Durable state of the post-merge gate, per project (spec 2026-10-05 §6.1, §6.3).
//!
//! One row of `postgate_state` (migration 0180) holds the last sha the gate passed on, the last
//! tip it was started for, the gate running right now, and the groups that are red with the sha
//! they have been red since. A restart finds the row as it was left, so the worker can resume or
//! requeue instead of starting over. This module is the table's only writer.
//!
//! It also names the worktree the gate runs in, `postgate-<project>` under the worktree root.
//! That directory is protected from the orphan sweep by NAME, like `integration-`:
//! `worktree::owner_from_dir_name` claims only `run-`, `job-` and `item-`, so nothing here needs
//! an `Owner` variant or a `worktrees` row.
//!
//! Driven by `verify_postgate_worker` (F3-2).

use std::path::{Path, PathBuf};

use sqlx::SqlitePool;

use crate::verify_batch::TargetState;
use crate::verify_bisect::{Probe, Verdict};

/// Directory-name prefix of the post-merge gate's worktree.
pub const POSTGATE_PREFIX: &str = "postgate-";

/// Where the post-merge gate checks out the target for `project_root`. Named like
/// `git_exec::integration_worktree`, including its known limit: two projects whose directories
/// share a leaf name collide under a shared `NUCLEOS_WORKTREE_ROOT`.
pub fn postgate_worktree(project_root: &Path) -> PathBuf {
    let project = project_root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "unnamed".to_owned());
    crate::worktree::worktree_root(project_root).join(format!("{POSTGATE_PREFIX}{project}"))
}

/// The feed kind of the owner-facing report of a confirmed red (spec 2026-10-05 §6.2 step 4).
pub const POSTGATE_RED_KIND: &str = "postgate_red";

/// Which step of handling a red the project is in. `None` in `State::red_phase` means no red is
/// being handled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedPhase {
    /// The gate is being repeated on the red sha, to tell an unstable test from a broken target.
    FlakeCheck,
    /// The first-parent merges of `(red_base_sha, red_sha]` are being probed one at a time.
    Bisect,
}

impl RedPhase {
    fn from_text(text: &str) -> Option<RedPhase> {
        match text {
            "flake_check" => Some(RedPhase::FlakeCheck),
            "bisect" => Some(RedPhase::Bisect),
            _ => None,
        }
    }
}

fn probe_text(probe: Probe) -> &'static str {
    match probe {
        Probe::Green => "green",
        Probe::Red => "red",
        Probe::Inconclusive => "inconclusive",
    }
}

/// An unknown word reads as `Inconclusive`: the bisection skips it instead of trusting it.
fn probe_from_text(text: &str) -> Probe {
    match text {
        "green" => Probe::Green,
        "red" => Probe::Red,
        _ => Probe::Inconclusive,
    }
}

/// One project's post-merge gate state, as stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct State {
    pub project_id: String,
    pub target: String,
    pub last_green_sha: Option<String>,
    pub last_attempted_sha: Option<String>,
    pub running_sha: Option<String>,
    pub running_request_id: Option<i64>,
    pub red_groups: Vec<String>,
    pub red_since_sha: Option<String>,
    /// The step of handling a red this project is in, if any.
    pub red_phase: Option<RedPhase>,
    /// The sha whose gate went red and is being handled.
    pub red_sha: Option<String>,
    /// The last green sha when the red happened: the exclusive start of the bisected range.
    pub red_base_sha: Option<String>,
    /// The bisection probe claimed right now, and the verify ticket that runs it.
    pub probe_sha: Option<String>,
    pub probe_request_id: Option<i64>,
    /// Every probe result recorded so far, in the order it was recorded.
    pub probes: Vec<(String, Probe)>,
    /// The red sha the owner was last told about, and what that report found.
    pub reported_sha: Option<String>,
    pub culprit_sha: Option<String>,
    pub candidates: Vec<String>,
    pub also_suspect: Vec<String>,
}

impl State {
    /// A gate is running for this project.
    pub fn running(&self) -> bool {
        self.running_sha.is_some()
    }

    /// The view `verify_batch::decide` reads, for the target's current `tip`.
    pub fn target_state<'a>(&'a self, tip: &'a str) -> TargetState<'a> {
        TargetState {
            tip,
            last_green: self.last_green_sha.as_deref(),
            last_attempted: self.last_attempted_sha.as_deref(),
            running: self.running(),
        }
    }
}

#[derive(sqlx::FromRow)]
struct Row {
    project_id: String,
    target: String,
    last_green_sha: Option<String>,
    last_attempted_sha: Option<String>,
    running_sha: Option<String>,
    running_request_id: Option<i64>,
    red_groups: String,
    red_since_sha: Option<String>,
    red_phase: Option<String>,
    red_sha: Option<String>,
    red_base_sha: Option<String>,
    probe_sha: Option<String>,
    probe_request_id: Option<i64>,
    probes: String,
    reported_sha: Option<String>,
    culprit_sha: Option<String>,
    candidates: String,
    also_suspect: String,
}

/// A JSON column that cannot be decoded reads as empty, with a warning, like `red_groups`.
fn decode_list<T: serde::de::DeserializeOwned>(project: &str, column: &str, raw: &str) -> Vec<T> {
    serde_json::from_str(raw).unwrap_or_else(|error| {
        tracing::warn!(
            project,
            column,
            %error,
            "undecodable column in postgate_state; reading it as empty"
        );
        Vec::new()
    })
}

impl From<Row> for State {
    fn from(row: Row) -> Self {
        let red_groups = serde_json::from_str(&row.red_groups).unwrap_or_else(|error| {
            tracing::warn!(
                project = %row.project_id,
                %error,
                "undecodable red_groups in postgate_state; reading it as empty"
            );
            Vec::new()
        });
        let red_phase = row.red_phase.as_deref().and_then(|text| {
            let phase = RedPhase::from_text(text);
            if phase.is_none() {
                tracing::warn!(
                    project = %row.project_id,
                    phase = text,
                    "unknown red_phase in postgate_state; reading it as none"
                );
            }
            phase
        });
        let probes = decode_list::<(String, String)>(&row.project_id, "probes", &row.probes)
            .into_iter()
            .map(|(sha, text)| (sha, probe_from_text(&text)))
            .collect();
        let candidates = decode_list(&row.project_id, "candidates", &row.candidates);
        let also_suspect = decode_list(&row.project_id, "also_suspect", &row.also_suspect);
        State {
            project_id: row.project_id,
            target: row.target,
            last_green_sha: row.last_green_sha,
            last_attempted_sha: row.last_attempted_sha,
            running_sha: row.running_sha,
            running_request_id: row.running_request_id,
            red_groups,
            red_since_sha: row.red_since_sha,
            red_phase,
            red_sha: row.red_sha,
            red_base_sha: row.red_base_sha,
            probe_sha: row.probe_sha,
            probe_request_id: row.probe_request_id,
            probes,
            reported_sha: row.reported_sha,
            culprit_sha: row.culprit_sha,
            candidates,
            also_suspect,
        }
    }
}

/// The stored state of `project_id`, or `None` when the gate has never touched it.
pub async fn load(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Option<State>> {
    let row = sqlx::query_as::<_, Row>(
        "SELECT project_id, target, last_green_sha, last_attempted_sha, running_sha, \
                running_request_id, red_groups, red_since_sha, red_phase, red_sha, red_base_sha, \
                probe_sha, probe_request_id, probes, reported_sha, culprit_sha, candidates, \
                also_suspect \
         FROM postgate_state WHERE project_id = ?",
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(State::from))
}

/// Records that a gate for `sha` started. Refused (`false`, nothing changed) while a gate is
/// already running for the project, or while a red is still being handled: one gate per project
/// (§6.3).
pub async fn start(
    pool: &SqlitePool,
    project_id: &str,
    target: &str,
    sha: &str,
    request_id: Option<i64>,
) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "INSERT INTO postgate_state \
             (project_id, target, last_attempted_sha, running_sha, running_request_id, \
              running_started_at) \
         VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP) \
         ON CONFLICT(project_id) DO UPDATE SET \
             target = excluded.target, \
             last_attempted_sha = excluded.last_attempted_sha, \
             running_sha = excluded.running_sha, \
             running_request_id = excluded.running_request_id, \
             running_started_at = CURRENT_TIMESTAMP, \
             updated_at = CURRENT_TIMESTAMP \
         WHERE postgate_state.running_sha IS NULL AND postgate_state.red_phase IS NULL",
    )
    .bind(project_id)
    .bind(target)
    .bind(sha)
    .bind(sha)
    .bind(request_id)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// The gate running for `sha` passed. A finish for a sha that is not the running one changes
/// nothing and returns `false`, so a late finish cannot overwrite newer state.
pub async fn finish_green(pool: &SqlitePool, project_id: &str, sha: &str) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE postgate_state SET \
             last_green_sha = ?, \
             running_sha = NULL, running_request_id = NULL, running_started_at = NULL, \
             red_groups = '[]', red_since_sha = NULL, \
             red_phase = NULL, red_sha = NULL, red_base_sha = NULL, probe_sha = NULL, \
             probe_request_id = NULL, probes = '[]', reported_sha = NULL, culprit_sha = NULL, \
             candidates = '[]', also_suspect = '[]', \
             updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND running_sha = ?",
    )
    .bind(sha)
    .bind(project_id)
    .bind(sha)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// The gate running for `sha` failed in `groups`. `red_since_sha` keeps the first red sha until a
/// green clears it. Opens the flake-check phase: `red_sha` is `sha` and `red_base_sha` the last
/// green sha, so the range to bisect is fixed now. Same compare-and-set as `finish_green`.
pub async fn finish_red(
    pool: &SqlitePool,
    project_id: &str,
    sha: &str,
    groups: &[String],
) -> sqlx::Result<bool> {
    let encoded = serde_json::to_string(groups).unwrap_or_else(|_| "[]".to_owned());
    let done = sqlx::query(
        "UPDATE postgate_state SET \
             running_sha = NULL, running_request_id = NULL, running_started_at = NULL, \
             red_groups = ?, \
             red_since_sha = COALESCE(red_since_sha, ?), \
             red_phase = 'flake_check', red_sha = ?, red_base_sha = last_green_sha, \
             probe_sha = NULL, probe_request_id = NULL, probes = '[]', \
             updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND running_sha = ?",
    )
    .bind(encoded)
    .bind(sha)
    .bind(sha)
    .bind(project_id)
    .bind(sha)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Stores the verify ticket of the gate running for `sha`. Refused (`false`) when `sha` is not
/// the running one, so a late ticket cannot attach itself to a gate that already finished.
pub async fn set_request(
    pool: &SqlitePool,
    project_id: &str,
    sha: &str,
    request_id: i64,
) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE postgate_state SET running_request_id = ?, updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND running_sha = ?",
    )
    .bind(request_id)
    .bind(project_id)
    .bind(sha)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Gives up the gate running for `sha` without a verdict: the slot is released and
/// `last_attempted_sha` stays, so the same tip is not retried; only a new merge is.
pub async fn abandon(pool: &SqlitePool, project_id: &str, sha: &str) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE postgate_state SET \
             running_sha = NULL, running_request_id = NULL, running_started_at = NULL, \
             updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND running_sha = ?",
    )
    .bind(project_id)
    .bind(sha)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Marks `sha` green without running a gate (`verify_batch`'s `MarkCovered`: the full gate
/// already passed on exactly that sha before it was published). Starts nothing and leaves a
/// running gate untouched.
pub async fn mark_covered(
    pool: &SqlitePool,
    project_id: &str,
    target: &str,
    sha: &str,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO postgate_state (project_id, target, last_green_sha) VALUES (?, ?, ?) \
         ON CONFLICT(project_id) DO UPDATE SET \
             target = excluded.target, \
             last_green_sha = excluded.last_green_sha, \
             red_groups = '[]', \
             red_since_sha = NULL, \
             red_phase = NULL, red_sha = NULL, red_base_sha = NULL, probe_sha = NULL, \
             probe_request_id = NULL, probes = '[]', reported_sha = NULL, culprit_sha = NULL, \
             candidates = '[]', also_suspect = '[]', \
             updated_at = CURRENT_TIMESTAMP",
    )
    .bind(project_id)
    .bind(target)
    .bind(sha)
    .execute(pool)
    .await?;
    Ok(())
}

/// The recheck on `red_sha` passed: the red was not the target's, so `red_sha` counts as green and
/// everything about the red is cleared. Nobody is told (§6.2 step 1). Refused (`false`) unless the
/// project is in the flake-check phase for exactly that sha.
pub async fn flake_green(pool: &SqlitePool, project_id: &str, red_sha: &str) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE postgate_state SET \
             last_green_sha = ?, \
             red_groups = '[]', red_since_sha = NULL, \
             red_phase = NULL, red_sha = NULL, red_base_sha = NULL, probe_sha = NULL, \
             probe_request_id = NULL, probes = '[]', reported_sha = NULL, culprit_sha = NULL, \
             candidates = '[]', also_suspect = '[]', \
             updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND red_phase = 'flake_check' AND red_sha = ?",
    )
    .bind(red_sha)
    .bind(project_id)
    .bind(red_sha)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// The recheck on `red_sha` did not clear it: move to the bisection with no probes recorded.
/// Refused (`false`) unless the project is in the flake-check phase for exactly that sha.
pub async fn begin_bisect(
    pool: &SqlitePool,
    project_id: &str,
    red_sha: &str,
) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE postgate_state SET \
             red_phase = 'bisect', probe_sha = NULL, probe_request_id = NULL, probes = '[]', \
             updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND red_phase = 'flake_check' AND red_sha = ?",
    )
    .bind(project_id)
    .bind(red_sha)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Claims `sha` as the probe being run. One probe at a time: refused (`false`) while another is
/// claimed, or when no red is being handled.
pub async fn claim_probe(pool: &SqlitePool, project_id: &str, sha: &str) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE postgate_state SET \
             probe_sha = ?, probe_request_id = NULL, updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND red_phase IS NOT NULL AND probe_sha IS NULL",
    )
    .bind(sha)
    .bind(project_id)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Stores the verify ticket of the probe claimed for `sha`. Refused (`false`) when `sha` is not
/// the claimed probe or it already has a ticket, so a restart follows the ticket instead of
/// submitting another.
pub async fn set_probe_request(
    pool: &SqlitePool,
    project_id: &str,
    sha: &str,
    request_id: i64,
) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE postgate_state SET probe_request_id = ?, updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND probe_sha = ? AND probe_request_id IS NULL",
    )
    .bind(request_id)
    .bind(project_id)
    .bind(sha)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Appends the result of the probe claimed for `sha` and frees the probe slot. Refused (`false`)
/// outside the bisect phase or for a sha that is not the claimed probe. The stored list is read and
/// rewritten in one transaction, and the write only lands if the list is still what was read.
pub async fn record_probe(
    pool: &SqlitePool,
    project_id: &str,
    sha: &str,
    probe: Probe,
) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    let stored: Option<String> = sqlx::query_scalar(
        "SELECT probes FROM postgate_state \
         WHERE project_id = ? AND red_phase = 'bisect' AND probe_sha = ?",
    )
    .bind(project_id)
    .bind(sha)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(stored) = stored else {
        return Ok(false);
    };
    let mut pairs: Vec<(String, String)> = decode_list(project_id, "probes", &stored);
    pairs.push((sha.to_owned(), probe_text(probe).to_owned()));
    let encoded = serde_json::to_string(&pairs).unwrap_or_else(|_| "[]".to_owned());
    let done = sqlx::query(
        "UPDATE postgate_state SET \
             probes = ?, probe_sha = NULL, probe_request_id = NULL, \
             updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND red_phase = 'bisect' AND probe_sha = ? AND probes = ?",
    )
    .bind(encoded)
    .bind(project_id)
    .bind(sha)
    .bind(&stored)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() == 0 {
        return Ok(false);
    }
    tx.commit().await?;
    Ok(true)
}

/// Stores the bisection's `verdict` for the red on `red_sha`, ends the red handling (the phase and
/// the probe slot are freed, `reported_sha` becomes `red_sha`) and writes one `postgate_red` feed
/// line with `summary`, all in one transaction: a crash never leaves a report without its state
/// change or the reverse. A culprit equal to the one already stored is kept but not announced
/// again, so a target that stays red does not repeat itself on every merge. Refused (`false`, and
/// nothing written) unless a red is being handled for exactly `red_sha`.
pub async fn report(
    pool: &SqlitePool,
    project_id: &str,
    red_sha: &str,
    verdict: &Verdict,
    summary: &str,
) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    let earlier: Option<Option<String>> = sqlx::query_scalar(
        "SELECT culprit_sha FROM postgate_state \
         WHERE project_id = ? AND red_phase IS NOT NULL AND red_sha = ?",
    )
    .bind(project_id)
    .bind(red_sha)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(earlier) = earlier else {
        return Ok(false);
    };
    let none: &[String] = &[];
    let (culprit, candidates, also_suspect) = match verdict {
        Verdict::NoCandidates => (None, none, none),
        Verdict::Culprit { sha, also_suspect } => {
            (Some(sha.as_str()), none, also_suspect.as_slice())
        }
        Verdict::Inconclusive {
            candidates,
            also_suspect,
        } => (None, candidates.as_slice(), also_suspect.as_slice()),
    };
    let encode = |list: &[String]| serde_json::to_string(list).unwrap_or_else(|_| "[]".to_owned());
    let done = sqlx::query(
        "UPDATE postgate_state SET \
             red_phase = NULL, probe_sha = NULL, probe_request_id = NULL, \
             reported_sha = ?, culprit_sha = ?, candidates = ?, also_suspect = ?, \
             updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND red_phase IS NOT NULL AND red_sha = ?",
    )
    .bind(red_sha)
    .bind(culprit)
    .bind(encode(candidates))
    .bind(encode(also_suspect))
    .bind(project_id)
    .bind(red_sha)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() == 0 {
        return Ok(false);
    }
    let repeats = culprit.is_some() && culprit == earlier.as_deref();
    if !repeats {
        crate::feed::append_on(
            &mut *tx,
            Some(project_id),
            POSTGATE_RED_KIND,
            summary,
            None,
            None,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify_batch::{Decision, Idle, decide};

    #[tokio::test]
    async fn a_fresh_database_has_no_postgate_state() {
        let pool = crate::testdb::fresh_pool().await;
        assert_eq!(load(&pool, "p").await.unwrap(), None);
        let rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM postgate_state")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(rows, 0);
    }

    #[tokio::test]
    async fn start_records_the_attempt_and_marks_a_gate_running() {
        let pool = crate::testdb::fresh_pool().await;
        assert!(start(&pool, "p", "master", "a1", Some(7)).await.unwrap());

        let s = load(&pool, "p").await.unwrap().expect("a row after start");
        assert_eq!(s.project_id, "p");
        assert_eq!(s.target, "master");
        assert_eq!(s.last_attempted_sha.as_deref(), Some("a1"));
        assert_eq!(s.running_sha.as_deref(), Some("a1"));
        assert_eq!(s.running_request_id, Some(7));
        assert_eq!(s.last_green_sha, None);
        assert!(s.running());
    }

    #[tokio::test]
    async fn a_second_start_while_one_is_running_is_refused() {
        let pool = crate::testdb::fresh_pool().await;
        assert!(start(&pool, "p", "master", "a1", Some(1)).await.unwrap());
        let before = load(&pool, "p").await.unwrap();

        assert!(!start(&pool, "p", "master", "b2", Some(2)).await.unwrap());
        assert_eq!(
            load(&pool, "p").await.unwrap(),
            before,
            "state must not change"
        );
    }

    #[tokio::test]
    async fn finish_green_moves_last_green_and_clears_running_and_reds() {
        let pool = crate::testdb::fresh_pool().await;
        // A red first, so there is something for the green to clear.
        start(&pool, "p", "master", "a1", None).await.unwrap();
        assert!(
            finish_red(&pool, "p", "a1", &["core".to_owned()])
                .await
                .unwrap()
        );
        start(&pool, "p", "master", "b2", Some(3)).await.unwrap();

        assert!(finish_green(&pool, "p", "b2").await.unwrap());

        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.last_green_sha.as_deref(), Some("b2"));
        assert_eq!(s.running_sha, None);
        assert_eq!(s.running_request_id, None);
        assert!(!s.running());
        assert!(s.red_groups.is_empty());
        assert_eq!(s.red_since_sha, None);
        assert_eq!(s.last_attempted_sha.as_deref(), Some("b2"));
    }

    #[tokio::test]
    async fn finish_red_records_the_groups_and_keeps_the_first_red_sha() {
        let pool = crate::testdb::fresh_pool().await;
        start(&pool, "p", "master", "a1", None).await.unwrap();
        let groups = vec!["core".to_owned(), "shell".to_owned()];
        assert!(finish_red(&pool, "p", "a1", &groups).await.unwrap());

        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.red_groups, groups);
        assert_eq!(s.red_since_sha.as_deref(), Some("a1"));
        assert_eq!(s.running_sha, None);
        assert_eq!(s.last_green_sha, None);

        // A second red keeps the first sha it has been red since.
        start(&pool, "p", "master", "b2", None).await.unwrap();
        let later = vec!["core".to_owned()];
        assert!(finish_red(&pool, "p", "b2", &later).await.unwrap());
        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.red_groups, later);
        assert_eq!(s.red_since_sha.as_deref(), Some("a1"));
    }

    #[tokio::test]
    async fn a_finish_for_a_sha_that_is_not_running_changes_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        start(&pool, "p", "master", "a1", Some(1)).await.unwrap();
        let before = load(&pool, "p").await.unwrap();

        assert!(!finish_green(&pool, "p", "zzz").await.unwrap());
        assert!(
            !finish_red(&pool, "p", "zzz", &["core".to_owned()])
                .await
                .unwrap()
        );
        assert_eq!(load(&pool, "p").await.unwrap(), before);

        // And with nothing running at all, or no row for the project.
        finish_green(&pool, "p", "a1").await.unwrap();
        let settled = load(&pool, "p").await.unwrap();
        assert!(!finish_green(&pool, "p", "a1").await.unwrap());
        assert!(!finish_red(&pool, "other", "a1", &[]).await.unwrap());
        assert_eq!(load(&pool, "p").await.unwrap(), settled);
    }

    #[tokio::test]
    async fn mark_covered_sets_last_green_without_starting_a_gate() {
        let pool = crate::testdb::fresh_pool().await;
        mark_covered(&pool, "p", "master", "c3").await.unwrap();

        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.last_green_sha.as_deref(), Some("c3"));
        assert_eq!(s.last_attempted_sha, None, "nothing was attempted");
        assert!(!s.running());
        assert!(s.red_groups.is_empty());
    }

    #[tokio::test]
    async fn the_stored_state_is_what_decide_consumes() {
        let pool = crate::testdb::fresh_pool().await;
        start(&pool, "p", "master", "b", None).await.unwrap();
        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(
            decide(&s.target_state("b"), &[]),
            Decision::Idle(Idle::Running)
        );

        assert!(finish_green(&pool, "p", "b").await.unwrap());
        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(
            decide(&s.target_state("b"), &[]),
            Decision::Idle(Idle::UpToDate)
        );
    }

    #[tokio::test]
    async fn set_request_records_the_ticket_only_for_the_running_sha() {
        let pool = crate::testdb::fresh_pool().await;
        start(&pool, "p", "master", "a1", None).await.unwrap();

        assert!(set_request(&pool, "p", "a1", 42).await.unwrap());
        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.running_request_id, Some(42));
        assert_eq!(s.running_sha.as_deref(), Some("a1"));

        // Another sha, or a project with no row, changes nothing.
        let before = load(&pool, "p").await.unwrap();
        assert!(!set_request(&pool, "p", "zzz", 99).await.unwrap());
        assert!(!set_request(&pool, "other", "a1", 99).await.unwrap());
        assert_eq!(load(&pool, "p").await.unwrap(), before);

        // Once the gate has finished nothing is running, so a late ticket is refused.
        finish_green(&pool, "p", "a1").await.unwrap();
        assert!(!set_request(&pool, "p", "a1", 7).await.unwrap());
        assert_eq!(
            load(&pool, "p").await.unwrap().unwrap().running_request_id,
            None
        );
    }

    #[tokio::test]
    async fn abandon_clears_running_and_keeps_the_attempt() {
        let pool = crate::testdb::fresh_pool().await;
        start(&pool, "p", "master", "a1", Some(5)).await.unwrap();

        // A different sha is not the running one: nothing changes.
        let before = load(&pool, "p").await.unwrap();
        assert!(!abandon(&pool, "p", "zzz").await.unwrap());
        assert_eq!(load(&pool, "p").await.unwrap(), before);

        assert!(abandon(&pool, "p", "a1").await.unwrap());
        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.running_sha, None);
        assert_eq!(s.running_request_id, None);
        assert!(!s.running());
        assert_eq!(s.last_attempted_sha.as_deref(), Some("a1"));
        assert_eq!(s.last_green_sha, None, "an abandoned gate measured nothing");
        assert!(s.red_groups.is_empty());

        // The same tip is not retried; only a new merge is.
        assert_eq!(
            decide(&s.target_state("a1"), &[]),
            Decision::Idle(Idle::UpToDate)
        );
        // Nothing running any more, so a second abandon is a no-op.
        assert!(!abandon(&pool, "p", "a1").await.unwrap());
    }

    /// A project whose gate went red on `red` with `base` as the last green sha.
    async fn red_project(pool: &SqlitePool, base: &str, red: &str) {
        mark_covered(pool, "p", "master", base).await.unwrap();
        assert!(start(pool, "p", "master", red, Some(1)).await.unwrap());
        assert!(
            finish_red(pool, "p", red, &["gate_command".to_owned()])
                .await
                .unwrap()
        );
    }

    async fn feed_lines(pool: &SqlitePool) -> Vec<(Option<String>, String, String)> {
        sqlx::query_as("SELECT project_id, kind, summary FROM feed ORDER BY id")
            .fetch_all(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_red_opens_the_flake_check_and_blocks_a_new_gate() {
        let pool = crate::testdb::fresh_pool().await;
        red_project(&pool, "g0", "r1").await;

        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.red_phase, Some(RedPhase::FlakeCheck));
        assert_eq!(s.red_sha.as_deref(), Some("r1"));
        assert_eq!(s.red_base_sha.as_deref(), Some("g0"));
        assert_eq!(s.probe_sha, None);
        assert_eq!(s.probe_request_id, None);
        assert!(s.probes.is_empty());
        assert!(!s.running());

        // One gate per project, and a red being handled counts: nothing new starts.
        let before = load(&pool, "p").await.unwrap();
        assert!(!start(&pool, "p", "master", "n2", Some(2)).await.unwrap());
        assert_eq!(load(&pool, "p").await.unwrap(), before);
        assert_eq!(POSTGATE_RED_KIND, "postgate_red");
    }

    #[tokio::test]
    async fn a_passing_recheck_counts_the_red_sha_green() {
        let pool = crate::testdb::fresh_pool().await;
        red_project(&pool, "g0", "r1").await;

        // Wrong sha, wrong project, or the wrong phase: nothing changes.
        let before = load(&pool, "p").await.unwrap();
        assert!(!flake_green(&pool, "p", "zzz").await.unwrap());
        assert!(!flake_green(&pool, "other", "r1").await.unwrap());
        assert_eq!(load(&pool, "p").await.unwrap(), before);
        assert!(begin_bisect(&pool, "p", "r1").await.unwrap());
        assert!(!flake_green(&pool, "p", "r1").await.unwrap());

        // A fresh red, rechecked green.
        mark_covered(&pool, "p", "master", "g0").await.unwrap();
        start(&pool, "p", "master", "r2", None).await.unwrap();
        finish_red(&pool, "p", "r2", &["gate_command".to_owned()])
            .await
            .unwrap();
        assert!(flake_green(&pool, "p", "r2").await.unwrap());

        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.last_green_sha.as_deref(), Some("r2"));
        assert_eq!(s.red_phase, None);
        assert_eq!(s.red_sha, None);
        assert_eq!(s.red_base_sha, None);
        assert!(s.red_groups.is_empty());
        assert_eq!(s.red_since_sha, None);
        assert!(feed_lines(&pool).await.is_empty(), "a flake tells nobody");
        // The slot is free again.
        assert!(start(&pool, "p", "master", "n3", None).await.unwrap());
    }

    #[tokio::test]
    async fn bisect_probes_are_claimed_recorded_and_reloaded_in_order() {
        use crate::verify_bisect::Probe;
        let pool = crate::testdb::fresh_pool().await;
        red_project(&pool, "g0", "r1").await;
        assert!(begin_bisect(&pool, "p", "r1").await.unwrap());
        assert_eq!(
            load(&pool, "p").await.unwrap().unwrap().red_phase,
            Some(RedPhase::Bisect)
        );

        // One probe at a time, and a late ticket attaches only to the claimed sha.
        assert!(claim_probe(&pool, "p", "m2").await.unwrap());
        assert!(!claim_probe(&pool, "p", "m3").await.unwrap());
        assert!(!set_probe_request(&pool, "p", "m3", 9).await.unwrap());
        assert!(set_probe_request(&pool, "p", "m2", 9).await.unwrap());
        assert!(!set_probe_request(&pool, "p", "m2", 10).await.unwrap());
        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.probe_sha.as_deref(), Some("m2"));
        assert_eq!(s.probe_request_id, Some(9));

        // Recording the wrong sha changes nothing; the right one frees the probe slot.
        assert!(!record_probe(&pool, "p", "m3", Probe::Red).await.unwrap());
        assert!(record_probe(&pool, "p", "m2", Probe::Green).await.unwrap());
        assert!(!record_probe(&pool, "p", "m2", Probe::Green).await.unwrap());
        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.probe_sha, None);
        assert_eq!(s.probe_request_id, None);

        assert!(claim_probe(&pool, "p", "m3").await.unwrap());
        assert!(
            record_probe(&pool, "p", "m3", Probe::Inconclusive)
                .await
                .unwrap()
        );
        assert!(claim_probe(&pool, "p", "m4").await.unwrap());
        assert!(record_probe(&pool, "p", "m4", Probe::Red).await.unwrap());

        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(
            s.probes,
            vec![
                ("m2".to_owned(), Probe::Green),
                ("m3".to_owned(), Probe::Inconclusive),
                ("m4".to_owned(), Probe::Red),
            ],
            "probes come back in the order they were recorded"
        );
        assert_eq!(s.red_phase, Some(RedPhase::Bisect));
    }

    #[tokio::test]
    async fn report_stores_the_verdict_writes_one_feed_line_and_frees_the_slot() {
        use crate::verify_bisect::Verdict;
        let pool = crate::testdb::fresh_pool().await;
        red_project(&pool, "g0", "r1").await;
        begin_bisect(&pool, "p", "r1").await.unwrap();

        let culprit = Verdict::Culprit {
            sha: "m3".to_owned(),
            also_suspect: vec!["m1".to_owned()],
        };
        // A report for another red sha is refused and writes nothing.
        assert!(!report(&pool, "p", "zzz", &culprit, "nope").await.unwrap());
        assert!(feed_lines(&pool).await.is_empty());

        assert!(
            report(&pool, "p", "r1", &culprit, "culprit m3")
                .await
                .unwrap()
        );
        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.red_phase, None);
        assert_eq!(s.probe_sha, None);
        assert_eq!(s.reported_sha.as_deref(), Some("r1"));
        assert_eq!(s.culprit_sha.as_deref(), Some("m3"));
        assert_eq!(s.also_suspect, vec!["m1".to_owned()]);
        assert_eq!(
            feed_lines(&pool).await,
            vec![(
                Some("p".to_owned()),
                POSTGATE_RED_KIND.to_owned(),
                "culprit m3".to_owned()
            )]
        );
        assert!(
            !report(&pool, "p", "r1", &culprit, "again").await.unwrap(),
            "the phase is gone, so a second report is refused"
        );
        assert_eq!(feed_lines(&pool).await.len(), 1);

        // The same culprit on a later red is stored but not announced twice.
        start(&pool, "p", "master", "r2", None).await.unwrap();
        finish_red(&pool, "p", "r2", &["gate_command".to_owned()])
            .await
            .unwrap();
        begin_bisect(&pool, "p", "r2").await.unwrap();
        assert!(
            report(&pool, "p", "r2", &culprit, "same again")
                .await
                .unwrap()
        );
        assert_eq!(feed_lines(&pool).await.len(), 1);
        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.reported_sha.as_deref(), Some("r2"));
        assert_eq!(s.red_phase, None);

        // An inconclusive verdict always announces, and a green clears the stored verdict.
        start(&pool, "p", "master", "r3", None).await.unwrap();
        finish_red(&pool, "p", "r3", &[]).await.unwrap();
        begin_bisect(&pool, "p", "r3").await.unwrap();
        let unsure = Verdict::Inconclusive {
            candidates: vec!["m4".to_owned(), "m5".to_owned()],
            also_suspect: Vec::new(),
        };
        assert!(report(&pool, "p", "r3", &unsure, "unsure").await.unwrap());
        assert_eq!(feed_lines(&pool).await.len(), 2);
        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.culprit_sha, None);
        assert_eq!(s.candidates, vec!["m4".to_owned(), "m5".to_owned()]);

        start(&pool, "p", "master", "g9", None).await.unwrap();
        assert!(finish_green(&pool, "p", "g9").await.unwrap());
        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.culprit_sha, None);
        assert!(s.candidates.is_empty());
        assert!(s.also_suspect.is_empty());
        assert_eq!(s.reported_sha, None);
    }

    #[test]
    fn the_postgate_worktree_is_named_by_the_project() {
        // The directory above is `worktree_root`, which reads an environment variable other tests
        // set, so only the leaf (which that variable cannot change) is asserted here.
        let root = std::path::Path::new("repos").join("alpha");
        let path = postgate_worktree(&root);
        assert_eq!(
            path.file_name().and_then(|n| n.to_str()),
            Some("postgate-alpha")
        );
        assert_eq!(POSTGATE_PREFIX, "postgate-");
    }
}
