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

use crate::verify::batch::TargetState;
use crate::verify::bisect::{Probe, Verdict};

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
    crate::execution::worktree::worktree_root(project_root)
        .join(format!("{POSTGATE_PREFIX}{project}"))
}

/// The feed kind of the owner-facing report of a confirmed red (spec 2026-10-05 §6.2 step 4).
pub const POSTGATE_RED_KIND: &str = "postgate_red";

/// The feed kind of a post-merge gate that did not start a full gate (F3-12): a tip whose tree a
/// daemon verification already passed, or a start held for the interval or a busy machine.
pub const POSTGATE_HELD_KIND: &str = "postgate_held";

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

    /// The stored word for this phase, the inverse of `from_text`.
    pub fn as_str(self) -> &'static str {
        match self {
            RedPhase::FlakeCheck => "flake_check",
            RedPhase::Bisect => "bisect",
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
    /// The culprit being reverted (F3-4), the vcs ticket of that revert, the commit the queue
    /// published for it, and the `fix/` branch built on that commit. All `None` unless
    /// `revert_on_red` started a revert.
    pub revert_merge_sha: Option<String>,
    pub revert_request_id: Option<i64>,
    pub revert_sha: Option<String>,
    pub fix_branch: Option<String>,
    /// The correction run of `fix_branch`: `None` until one is claimed, `Some(0)` while a claim has
    /// no run yet, the run's id afterwards.
    pub fix_run_id: Option<i64>,
    /// The newest tip proven by a daemon verification over a green-or-covered base (F3-12). It
    /// never replaces `last_green_sha`.
    pub covered_sha: Option<String>,
    /// Why a gate start is being held right now (`"interval"` / `"busy"`), until a gate starts.
    pub held_note: Option<String>,
    /// How many gates of `last_attempted_sha` ended without a verdict.
    pub unmeasured_runs: i64,
}

impl State {
    /// A gate is running for this project.
    pub fn running(&self) -> bool {
        self.running_sha.is_some()
    }

    /// `tip` was attempted, every gate of it ended without a verdict, and it is still within
    /// [`UNMEASURED_RETRIES`]: the worker gates it once more instead of calling it up to date.
    pub fn unmeasured_retry_due(&self, tip: &str) -> bool {
        self.last_attempted_sha.as_deref() == Some(tip)
            && !self.is_green(tip)
            && (1..=UNMEASURED_RETRIES).contains(&self.unmeasured_runs)
    }

    /// The post-land gate has judged `sha` green: it is the last green sha, or a tip a daemon
    /// verification covered. Pending, running and red are all "not green" - a push of the
    /// integration branch waits on this, and a red is answered by `revert_on_red` moving the branch
    /// to a revert, which is gated in turn and is what then gets pushed.
    pub fn is_green(&self, sha: &str) -> bool {
        self.last_green_sha.as_deref() == Some(sha) || self.covered_sha.as_deref() == Some(sha)
    }

    /// The note for a unit whose `key` (its group, or the gate command) is red on the target:
    /// `already failing on <target>@<sha>`. `None` when `key` is not red or no sha is known.
    pub fn already_failing(&self, key: &str) -> Option<String> {
        if !self.red_groups.iter().any(|g| g == key) {
            return None;
        }
        let sha = self.red_sha.as_ref().or(self.red_since_sha.as_ref())?;
        Some(format!("already failing on {}@{}", self.target, sha))
    }

    /// A revert was started and its correction run has not been claimed yet: a new culprit must not
    /// start a second revert over it.
    pub fn revert_in_flight(&self) -> bool {
        self.revert_merge_sha.is_some() && self.fix_run_id.is_none()
    }

    /// A revert was started and its `fix/` branch does not exist yet: the worker still has to
    /// queue the revert, follow it, or build the branch.
    pub fn revert_pending(&self) -> bool {
        self.revert_merge_sha.is_some() && self.fix_branch.is_none()
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
    revert_merge_sha: Option<String>,
    revert_request_id: Option<i64>,
    revert_sha: Option<String>,
    fix_branch: Option<String>,
    fix_run_id: Option<i64>,
    covered_sha: Option<String>,
    held_note: Option<String>,
    unmeasured_runs: i64,
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
            revert_merge_sha: row.revert_merge_sha,
            revert_request_id: row.revert_request_id,
            revert_sha: row.revert_sha,
            fix_branch: row.fix_branch,
            fix_run_id: row.fix_run_id,
            covered_sha: row.covered_sha,
            held_note: row.held_note,
            unmeasured_runs: row.unmeasured_runs,
        }
    }
}

/// The stored state of `project_id`, or `None` when the gate has never touched it.
pub async fn load(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Option<State>> {
    let row = sqlx::query_as::<_, Row>(
        "SELECT project_id, target, last_green_sha, last_attempted_sha, running_sha, \
                running_request_id, red_groups, red_since_sha, red_phase, red_sha, red_base_sha, \
                probe_sha, probe_request_id, probes, reported_sha, culprit_sha, candidates, \
                also_suspect, revert_merge_sha, revert_request_id, revert_sha, fix_branch, \
                fix_run_id, covered_sha, held_note, unmeasured_runs \
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
              running_started_at, last_started_at, submit_floor) \
         VALUES (?, ?, ?, ?, ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP, \
                 (SELECT COALESCE(MAX(id), 0) FROM verify_requests)) \
         ON CONFLICT(project_id) DO UPDATE SET \
             target = excluded.target, \
             unmeasured_runs = CASE \
                 WHEN postgate_state.last_attempted_sha = excluded.last_attempted_sha \
                 THEN postgate_state.unmeasured_runs ELSE 0 END, \
             last_attempted_sha = excluded.last_attempted_sha, \
             running_sha = excluded.running_sha, \
             running_request_id = excluded.running_request_id, \
             running_started_at = CURRENT_TIMESTAMP, \
             last_started_at = CURRENT_TIMESTAMP, \
             submit_floor = excluded.submit_floor, \
             held_note = NULL, \
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

/// A project's post-merge gate colour, as `postgate_transitions.to_state` spells it.
///
/// Red means `red_groups` is not empty: the same reading `State::red_groups` gives, and the one
/// thing every write below already sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Colour {
    Red,
    Green,
}

impl Colour {
    fn as_str(self) -> &'static str {
        match self {
            Colour::Red => "red",
            Colour::Green => "green",
        }
    }
}

/// `postgate_transitions.reason` values (spec §10).
const REASON_GATE: &str = "gate";
const REASON_FLAKE: &str = "flake";
const REASON_COVERED: &str = "covered";

/// What a colour-changing write reads inside its transaction, before its UPDATE.
struct Before {
    red: bool,
    /// The red being closed ended with the daemon's revert of its culprit published: a revert
    /// was started for exactly the culprit (`revert_merge_sha = culprit_sha`) and the queue
    /// recorded its commit (`revert_sha`). Only read by the writes that turn a project green.
    reverted: bool,
}

/// `postgate_state`: red_groups, culprit_sha, revert_merge_sha, revert_sha.
type BeforeRow = (String, Option<String>, Option<String>, Option<String>);

async fn read_before(conn: &mut sqlx::SqliteConnection, project_id: &str) -> sqlx::Result<Before> {
    let row: Option<BeforeRow> = sqlx::query_as(
        "SELECT red_groups, culprit_sha, revert_merge_sha, revert_sha \
         FROM postgate_state WHERE project_id = ?",
    )
    .bind(project_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(match row {
        // No row yet: a project the gate never judged is green.
        None => Before {
            red: false,
            reverted: false,
        },
        Some((groups, culprit, revert_merge, revert_sha)) => Before {
            red: !decode_list::<String>(project_id, "red_groups", &groups).is_empty(),
            reverted: revert_sha.is_some() && culprit.is_some() && culprit == revert_merge,
        },
    })
}

/// Appends the change to `postgate_transitions` when the colour moved, inside the caller's
/// transaction (spec analytics-pipeline §5.4). `Ok(false)`, nothing written, when it did not.
///
/// Called only after the caller's compare-and-set UPDATE affected a row, so a refused or
/// repeated call never reaches it and cannot duplicate a row.
// Eight plain columns of one row; a struct used once would only rename them.
#[allow(clippy::too_many_arguments)]
pub async fn record_transition(
    conn: &mut sqlx::SqliteConnection,
    project_id: &str,
    was_red: bool,
    to: Colour,
    sha: Option<&str>,
    groups: &[String],
    reason: &str,
    reverted: bool,
) -> sqlx::Result<bool> {
    if was_red == (to == Colour::Red) {
        return Ok(false);
    }
    let encoded = serde_json::to_string(groups).unwrap_or_else(|_| "[]".to_owned());
    sqlx::query(
        "INSERT INTO postgate_transitions \
             (project_id, at, to_state, sha, red_groups, reason, reverted) \
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(project_id)
    .bind(chrono::Utc::now().to_rfc3339())
    .bind(to.as_str())
    .bind(sha)
    .bind(encoded)
    .bind(reason)
    .bind(i64::from(reverted))
    .execute(&mut *conn)
    .await?;
    Ok(true)
}

/// The gate running for `sha` passed. A finish for a sha that is not the running one changes
/// nothing and returns `false`, so a late finish cannot overwrite newer state. Writes the
/// `→ green` transition in the same transaction when the project was red (spec
/// analytics-pipeline §5.4).
pub async fn finish_green(pool: &SqlitePool, project_id: &str, sha: &str) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    let before = read_before(&mut tx, project_id).await?;
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
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() == 0 {
        // Dropping `tx` rolls back the (read-only) transaction.
        return Ok(false);
    }
    record_transition(
        &mut tx,
        project_id,
        before.red,
        Colour::Green,
        Some(sha),
        &[],
        REASON_GATE,
        before.reverted,
    )
    .await?;
    tx.commit().await?;
    // After the commit, so a released push re-reads a state that already says green.
    crate::execution::vcs::release_awaiting_gate(pool, project_id).await;
    Ok(true)
}

/// The gate running for `sha` failed in `groups`. `red_since_sha` keeps the first red sha until a
/// green clears it. Opens the flake-check phase: `red_sha` is `sha` and `red_base_sha` the last
/// green sha, so the range to bisect is fixed now. Same compare-and-set as `finish_green`. Writes
/// the `→ red` transition in the same transaction when the project was green (spec
/// analytics-pipeline §5.4).
pub async fn finish_red(
    pool: &SqlitePool,
    project_id: &str,
    sha: &str,
    groups: &[String],
) -> sqlx::Result<bool> {
    let encoded = serde_json::to_string(groups).unwrap_or_else(|_| "[]".to_owned());
    let mut tx = pool.begin().await?;
    let before = read_before(&mut tx, project_id).await?;
    let done = sqlx::query(
        "UPDATE postgate_state SET \
             running_sha = NULL, running_request_id = NULL, running_started_at = NULL, \
             red_groups = ?, \
             red_since_sha = COALESCE(red_since_sha, ?), \
             red_phase = 'flake_check', red_sha = ?, red_base_sha = last_green_sha, \
             probe_sha = NULL, probe_request_id = NULL, probes = '[]', \
             covered_sha = NULL, \
             updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND running_sha = ?",
    )
    .bind(encoded)
    .bind(sha)
    .bind(sha)
    .bind(project_id)
    .bind(sha)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() == 0 {
        // Dropping `tx` rolls back the (read-only) transaction.
        return Ok(false);
    }
    // An empty group list stores `red_groups = '[]'`: the colour stays green.
    let to = if groups.is_empty() {
        Colour::Green
    } else {
        Colour::Red
    };
    record_transition(
        &mut tx,
        project_id,
        before.red,
        to,
        Some(sha),
        groups,
        REASON_GATE,
        false,
    )
    .await?;
    tx.commit().await?;
    Ok(true)
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

/// How many more gates a tip gets after its gate ended without a verdict. One: a timeout on a
/// loaded machine is worth a second run, and a gate that errors every time must not loop.
pub const UNMEASURED_RETRIES: i64 = 1;

/// Like [`abandon`], for a gate whose ticket finished without a verdict: it also counts the run in
/// `unmeasured_runs`, which earns the tip [`UNMEASURED_RETRIES`] more gates.
pub async fn abandon_unmeasured(
    pool: &SqlitePool,
    project_id: &str,
    sha: &str,
) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE postgate_state SET \
             running_sha = NULL, running_request_id = NULL, running_started_at = NULL, \
             unmeasured_runs = unmeasured_runs + 1, updated_at = CURRENT_TIMESTAMP \
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
/// running gate untouched. Writes the `→ green` transition in the same transaction when the
/// project was red (spec analytics-pipeline §5.4).
pub async fn mark_covered(
    pool: &SqlitePool,
    project_id: &str,
    target: &str,
    sha: &str,
) -> sqlx::Result<()> {
    let mut tx = pool.begin().await?;
    let before = read_before(&mut tx, project_id).await?;
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
    .execute(&mut *tx)
    .await?;
    // The upsert always affects one row; `record_transition` skips green to green itself.
    record_transition(
        &mut tx,
        project_id,
        before.red,
        Colour::Green,
        Some(sha),
        &[],
        REASON_COVERED,
        before.reverted,
    )
    .await?;
    tx.commit().await?;
    crate::execution::vcs::release_awaiting_gate(pool, project_id).await;
    Ok(())
}

/// Records that `sha`, a new tip of the target, is covered by a daemon verification of its exact
/// tree that passed over `base` (F3-12, decisions 4 and 5), and tells the feed once. `base` must be
/// the last full green or the previous covered tip (a compare-and-set in SQL, so a stale proof
/// covers nothing); a cover chains by induction. It never moves `last_green_sha`: a later red is
/// bisected from the last COMPLETE green, so a wrong cover costs a longer bisection and never a
/// lost red. Never inserts a row, and refuses (`false`, nothing written) while a gate runs, while a
/// red is being handled, or when `sha` was already attempted or covered.
pub async fn settle_covered(
    pool: &SqlitePool,
    project_id: &str,
    target: &str,
    sha: &str,
    base: &str,
    summary: &str,
) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    let done = sqlx::query(
        "UPDATE postgate_state SET \
             covered_sha = ?1, last_attempted_sha = ?1, held_note = NULL, unmeasured_runs = 0, \
             updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ?2 AND target = ?3 \
           AND running_sha IS NULL AND red_phase IS NULL \
           AND (last_green_sha = ?4 OR covered_sha = ?4) \
           AND (last_attempted_sha IS NULL OR last_attempted_sha != ?1)",
    )
    .bind(sha)
    .bind(project_id)
    .bind(target)
    .bind(base)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() == 0 {
        return Ok(false);
    }
    crate::feed::append_on(
        &mut tx,
        Some(project_id),
        POSTGATE_HELD_KIND,
        summary,
        None,
        None,
    )
    .await?;
    tx.commit().await?;
    crate::execution::vcs::release_awaiting_gate(pool, project_id).await;
    Ok(true)
}

/// Records that the start of a gate for `target` is being held for `reason` (`"interval"` or
/// `"busy"`) and tells the feed: one line per reason and episode. `start` clears the note, so the
/// next hold after a gate speaks again. Returns `true` when the reason changed (a line was written).
pub async fn hold(
    pool: &SqlitePool,
    project_id: &str,
    target: &str,
    reason: &str,
    summary: &str,
) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    let done = sqlx::query(
        "INSERT INTO postgate_state (project_id, target, held_note) VALUES (?, ?, ?) \
         ON CONFLICT(project_id) DO UPDATE SET \
             held_note = excluded.held_note, \
             updated_at = CURRENT_TIMESTAMP \
         WHERE postgate_state.held_note IS NOT excluded.held_note",
    )
    .bind(project_id)
    .bind(target)
    .bind(reason)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() == 0 {
        return Ok(false);
    }
    crate::feed::append_on(
        &mut tx,
        Some(project_id),
        POSTGATE_HELD_KIND,
        summary,
        None,
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Seconds since the latest gate of `project_id` started, or `None` when none ever did (a row
/// written only by a hold has no start).
pub async fn seconds_since_last_start(
    pool: &SqlitePool,
    project_id: &str,
) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar(
        "SELECT CAST((julianday('now') - julianday(last_started_at)) * 86400 AS INTEGER) \
         FROM postgate_state WHERE project_id = ? AND last_started_at IS NOT NULL",
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await
}

/// The recheck on `red_sha` passed: the red was not the target's, so `red_sha` counts as green and
/// everything about the red is cleared. Nobody is told (§6.2 step 1). Refused (`false`) unless the
/// project is in the flake-check phase for exactly that sha. Writes the `→ green` transition in
/// the same transaction when the project was red (spec analytics-pipeline §5.4).
pub async fn flake_green(pool: &SqlitePool, project_id: &str, red_sha: &str) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    let before = read_before(&mut tx, project_id).await?;
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
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() == 0 {
        return Ok(false);
    }
    record_transition(
        &mut tx,
        project_id,
        before.red,
        Colour::Green,
        Some(red_sha),
        &[],
        REASON_FLAKE,
        before.reverted,
    )
    .await?;
    tx.commit().await?;
    crate::execution::vcs::release_awaiting_gate(pool, project_id).await;
    Ok(true)
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
             probe_sha = ?, probe_request_id = NULL, updated_at = CURRENT_TIMESTAMP, \
             submit_floor = (SELECT COALESCE(MAX(id), 0) FROM verify_requests) \
         WHERE project_id = ? AND red_phase IS NOT NULL AND probe_sha IS NULL",
    )
    .bind(sha)
    .bind(project_id)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// The verify request already submitted under the claim `project_id` holds right now (its running
/// gate or its claimed probe), when the ticket id was never stored: a daemon that died between the
/// submit and `set_request` / `set_probe_request`.
///
/// Every claim records `submit_floor`, the newest request id at that moment, and this worker is the
/// only submitter of `postgate` requests for a project (one gate or probe at a time). So the oldest
/// postgate request newer than the floor is the one that claim submitted. `None` when there is no
/// such request, and when the claim predates the column.
pub async fn orphaned_request(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Option<i64>> {
    sqlx::query_scalar(
        "SELECT v.id FROM verify_requests v \
           JOIN postgate_state s ON s.project_id = v.project_id \
          WHERE v.project_id = ? AND v.caller = 'postgate' \
            AND v.kind = 'test' AND v.scope = 'full' \
            AND s.submit_floor IS NOT NULL AND v.id > s.submit_floor \
            AND (s.running_sha IS NOT NULL OR s.probe_sha IS NOT NULL) \
          ORDER BY v.id LIMIT 1",
    )
    .bind(project_id)
    .fetch_optional(pool)
    .await
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
    report_inner(pool, project_id, red_sha, verdict, summary, false).await
}

/// `report`, and when the verdict is a culprit that is not a repeat, the revert of that culprit is
/// started in the same transaction: `revert_merge_sha` is set and the other four revert columns are
/// cleared. The feed line therefore never says "reverting" without the state to back it, or the
/// reverse. A repeated culprit starts nothing. The caller decides whether a revert may start at all
/// (the switch, and none already in flight); this only writes it.
pub async fn report_with_revert(
    pool: &SqlitePool,
    project_id: &str,
    red_sha: &str,
    verdict: &Verdict,
    summary: &str,
) -> sqlx::Result<bool> {
    report_inner(pool, project_id, red_sha, verdict, summary, true).await
}

async fn report_inner(
    pool: &SqlitePool,
    project_id: &str,
    red_sha: &str,
    verdict: &Verdict,
    summary: &str,
    revert: bool,
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
    if revert
        && !repeats
        && let Some(culprit) = culprit
    {
        sqlx::query(
            "UPDATE postgate_state SET \
                 revert_merge_sha = ?, revert_request_id = NULL, revert_sha = NULL, \
                 fix_branch = NULL, fix_run_id = NULL \
             WHERE project_id = ?",
        )
        .bind(culprit)
        .bind(project_id)
        .execute(&mut *tx)
        .await?;
    }
    if !repeats {
        crate::feed::append_on(
            &mut tx,
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

/// Stores the vcs ticket of the revert of `merge_sha`. Refused (`false`) when no revert of that
/// culprit is started, so a late ticket cannot attach itself to a revert that already ended.
pub async fn set_revert_request(
    pool: &SqlitePool,
    project_id: &str,
    merge_sha: &str,
    request_id: i64,
) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE postgate_state SET revert_request_id = ?, updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND revert_merge_sha = ?",
    )
    .bind(request_id)
    .bind(project_id)
    .bind(merge_sha)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// Stores the commit the queue published for the revert of `merge_sha`. Same compare-and-set as
/// `set_revert_request`.
pub async fn record_revert(
    pool: &SqlitePool,
    project_id: &str,
    merge_sha: &str,
    revert_sha: &str,
) -> sqlx::Result<bool> {
    let done = sqlx::query(
        "UPDATE postgate_state SET revert_sha = ?, updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND revert_merge_sha = ?",
    )
    .bind(revert_sha)
    .bind(project_id)
    .bind(merge_sha)
    .execute(pool)
    .await?;
    Ok(done.rows_affected() > 0)
}

/// The revert of `merge_sha` did not happen, or happened and left no branch: the five revert
/// columns are cleared so the state is free again, and `summary` goes to the feed, in one
/// transaction. Refused (`false`, nothing written) when no revert of that culprit is started.
pub async fn fail_revert(
    pool: &SqlitePool,
    project_id: &str,
    merge_sha: &str,
    summary: &str,
) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    let done = sqlx::query(
        "UPDATE postgate_state SET \
             revert_merge_sha = NULL, revert_request_id = NULL, revert_sha = NULL, \
             fix_branch = NULL, fix_run_id = NULL, updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND revert_merge_sha = ?",
    )
    .bind(project_id)
    .bind(merge_sha)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() == 0 {
        return Ok(false);
    }
    crate::feed::append_on(
        &mut tx,
        Some(project_id),
        POSTGATE_RED_KIND,
        summary,
        None,
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

/// Stores the `fix/` branch built on the revert of `merge_sha` and tells the owner (`summary`), in
/// one transaction. Refused (`false`, nothing written) when no revert of that culprit is started.
pub async fn set_fix_branch(
    pool: &SqlitePool,
    project_id: &str,
    merge_sha: &str,
    branch: &str,
    summary: &str,
) -> sqlx::Result<bool> {
    let mut tx = pool.begin().await?;
    let done = sqlx::query(
        "UPDATE postgate_state SET fix_branch = ?, updated_at = CURRENT_TIMESTAMP \
         WHERE project_id = ? AND revert_merge_sha = ?",
    )
    .bind(branch)
    .bind(project_id)
    .bind(merge_sha)
    .execute(&mut *tx)
    .await?;
    if done.rows_affected() == 0 {
        return Ok(false);
    }
    crate::feed::append_on(
        &mut tx,
        Some(project_id),
        POSTGATE_RED_KIND,
        summary,
        None,
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::batch::{Decision, Idle, decide};

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
    async fn an_unmeasured_gate_earns_its_tip_one_retry_and_a_new_tip_starts_from_zero() {
        let pool = crate::testdb::fresh_pool().await;
        assert!(start(&pool, "p", "master", "a1", Some(1)).await.unwrap());
        assert!(abandon_unmeasured(&pool, "p", "a1").await.unwrap());
        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.unmeasured_runs, 1);
        assert!(s.unmeasured_retry_due("a1"));
        assert!(
            !s.unmeasured_retry_due("b2"),
            "only the attempted tip is retried"
        );

        // The retry of the same tip keeps the count; a second unmeasured run spends the budget.
        assert!(start(&pool, "p", "master", "a1", Some(2)).await.unwrap());
        assert!(abandon_unmeasured(&pool, "p", "a1").await.unwrap());
        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.unmeasured_runs, 2);
        assert!(!s.unmeasured_retry_due("a1"));

        assert!(start(&pool, "p", "master", "b2", Some(3)).await.unwrap());
        assert_eq!(load(&pool, "p").await.unwrap().unwrap().unmeasured_runs, 0);
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

    /// Ends the handling of the red for `red_sha` with an empty verdict, the smallest legitimate
    /// way to free the slot while keeping `red_since_sha` and `red_groups`.
    async fn end_red(pool: &SqlitePool, red_sha: &str) {
        let verdict = crate::verify::bisect::Verdict::NoCandidates;
        assert!(
            report(pool, "p", red_sha, &verdict, "no candidates")
                .await
                .unwrap()
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
        // Handling the red ends with a report, which frees the slot for the next gate.
        end_red(&pool, "a1").await;
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

        // A second red keeps the first sha it has been red since. The first red's handling has to
        // end (a report) before the next gate may start.
        end_red(&pool, "a1").await;
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
        use crate::verify::bisect::Probe;
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
        use crate::verify::bisect::Verdict;
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

    #[test]
    fn already_failing_annotates_only_a_red_group_with_the_target_and_red_sha() {
        let mut state = State {
            project_id: "p".to_owned(),
            target: "main".to_owned(),
            last_green_sha: None,
            last_attempted_sha: None,
            running_sha: None,
            running_request_id: None,
            red_groups: vec!["core".to_owned()],
            red_since_sha: Some("since0".to_owned()),
            red_phase: None,
            red_sha: Some("red1".to_owned()),
            red_base_sha: None,
            probe_sha: None,
            probe_request_id: None,
            probes: Vec::new(),
            reported_sha: None,
            culprit_sha: None,
            candidates: Vec::new(),
            also_suspect: Vec::new(),
            revert_merge_sha: None,
            revert_request_id: None,
            revert_sha: None,
            fix_branch: None,
            fix_run_id: None,
            covered_sha: None,
            held_note: None,
            unmeasured_runs: 0,
        };
        assert_eq!(
            state.already_failing("core").as_deref(),
            Some("already failing on main@red1")
        );
        assert_eq!(state.already_failing("py"), None);

        // Without a confirmed red sha the red-since sha is the fallback.
        state.red_sha = None;
        assert_eq!(
            state.already_failing("core").as_deref(),
            Some("already failing on main@since0")
        );

        // With neither there is no sha to cite, so no annotation.
        state.red_since_sha = None;
        assert_eq!(state.already_failing("core"), None);

        assert_eq!(RedPhase::FlakeCheck.as_str(), "flake_check");
        assert_eq!(RedPhase::Bisect.as_str(), "bisect");
    }

    /// F3-12: how many `postgate_held` lines the feed holds.
    async fn held_lines(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = 'postgate_held'")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_cover_is_recorded_only_on_a_green_or_covered_base_and_never_moves_the_green() {
        let pool = crate::testdb::fresh_pool().await;
        mark_covered(&pool, "p", "main", "g").await.unwrap();

        // A base that is neither the last green nor the covered sha covers nothing.
        assert!(
            !settle_covered(&pool, "p", "main", "c1", "x", "no")
                .await
                .unwrap()
        );
        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.covered_sha, None);
        assert_eq!(
            s.last_attempted_sha, None,
            "a refused cover attempts nothing"
        );

        // On the last green it does, and the green stays where it was.
        assert!(
            settle_covered(&pool, "p", "main", "c1", "g", "covered c1")
                .await
                .unwrap()
        );
        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.covered_sha.as_deref(), Some("c1"));
        assert_eq!(s.last_green_sha.as_deref(), Some("g"));
        assert_eq!(s.last_attempted_sha.as_deref(), Some("c1"));

        // A chain: the next cover sits on the previous covered sha, the green still stays.
        assert!(
            settle_covered(&pool, "p", "main", "c2", "c1", "covered c2")
                .await
                .unwrap()
        );
        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.covered_sha.as_deref(), Some("c2"));
        assert_eq!(s.last_green_sha.as_deref(), Some("g"));

        // Only the most recent covered sha chains: c1 is no longer a base, the green is.
        assert!(
            !settle_covered(&pool, "p", "main", "c3", "c1", "covered c3")
                .await
                .unwrap()
        );
        assert_eq!(
            load(&pool, "p")
                .await
                .unwrap()
                .unwrap()
                .covered_sha
                .as_deref(),
            Some("c2")
        );

        // It never inserts a row for a project the gate has not seen.
        assert!(
            !settle_covered(&pool, "other", "main", "c1", "g", "no")
                .await
                .unwrap()
        );
        assert_eq!(load(&pool, "other").await.unwrap(), None);

        // And never while a gate is running.
        assert!(start(&pool, "p", "main", "r1", Some(1)).await.unwrap());
        assert!(
            !settle_covered(&pool, "p", "main", "c4", "g", "no")
                .await
                .unwrap()
        );
        assert_eq!(
            load(&pool, "p")
                .await
                .unwrap()
                .unwrap()
                .covered_sha
                .as_deref(),
            Some("c2")
        );
    }

    #[tokio::test]
    async fn settling_a_covered_tip_tells_the_feed_once() {
        let pool = crate::testdb::fresh_pool().await;
        mark_covered(&pool, "p", "main", "g").await.unwrap();
        assert_eq!(held_lines(&pool).await, 0);

        assert!(
            settle_covered(&pool, "p", "main", "c1", "g", "tip c1 was not gated")
                .await
                .unwrap()
        );
        assert_eq!(held_lines(&pool).await, 1);
        let lines = feed_lines(&pool).await;
        assert_eq!(
            lines,
            vec![(
                Some("p".to_owned()),
                POSTGATE_HELD_KIND.to_owned(),
                "tip c1 was not gated".to_owned()
            )]
        );

        // The same tip again changes nothing and writes nothing.
        assert!(
            !settle_covered(&pool, "p", "main", "c1", "g", "tip c1 was not gated")
                .await
                .unwrap()
        );
        assert_eq!(held_lines(&pool).await, 1);
        assert_eq!(POSTGATE_HELD_KIND, "postgate_held");
    }

    #[tokio::test]
    async fn a_confirmed_red_forgets_the_covered_sha() {
        let pool = crate::testdb::fresh_pool().await;
        mark_covered(&pool, "p", "main", "g").await.unwrap();
        assert!(
            settle_covered(&pool, "p", "main", "c1", "g", "covered c1")
                .await
                .unwrap()
        );
        assert!(start(&pool, "p", "main", "r1", Some(1)).await.unwrap());
        // The cover survives the start of a gate; only a confirmed red drops it.
        assert_eq!(
            load(&pool, "p")
                .await
                .unwrap()
                .unwrap()
                .covered_sha
                .as_deref(),
            Some("c1")
        );

        assert!(
            finish_red(&pool, "p", "r1", &["gate_command".to_owned()])
                .await
                .unwrap()
        );
        let s = load(&pool, "p").await.unwrap().unwrap();
        assert_eq!(s.covered_sha, None);
        assert_eq!(s.last_green_sha.as_deref(), Some("g"));
        assert_eq!(s.red_base_sha.as_deref(), Some("g"));
    }

    #[tokio::test]
    async fn a_hold_writes_one_feed_line_per_reason_until_a_gate_starts() {
        let pool = crate::testdb::fresh_pool().await;

        assert!(
            hold(&pool, "p", "main", "interval", "waiting for the interval")
                .await
                .unwrap()
        );
        assert_eq!(held_lines(&pool).await, 1);
        assert_eq!(
            load(&pool, "p")
                .await
                .unwrap()
                .unwrap()
                .held_note
                .as_deref(),
            Some("interval")
        );

        // The same reason again is the same episode: no new line.
        assert!(
            !hold(&pool, "p", "main", "interval", "waiting for the interval")
                .await
                .unwrap()
        );
        assert_eq!(held_lines(&pool).await, 1);

        // A different reason is news.
        assert!(
            hold(&pool, "p", "main", "busy", "waiting for an idle machine")
                .await
                .unwrap()
        );
        assert_eq!(held_lines(&pool).await, 2);
        assert!(
            !hold(&pool, "p", "main", "busy", "waiting for an idle machine")
                .await
                .unwrap()
        );
        assert_eq!(held_lines(&pool).await, 2);

        // A gate starting ends the episode: the note is cleared, and a later hold speaks again.
        assert!(start(&pool, "p", "main", "a1", None).await.unwrap());
        assert_eq!(load(&pool, "p").await.unwrap().unwrap().held_note, None);
        assert!(finish_green(&pool, "p", "a1").await.unwrap());
        assert!(
            hold(&pool, "p", "main", "busy", "waiting for an idle machine")
                .await
                .unwrap()
        );
        assert_eq!(held_lines(&pool).await, 3);
    }

    #[tokio::test]
    async fn start_records_when_the_last_gate_started() {
        let pool = crate::testdb::fresh_pool().await;
        // No row, and a row written by a hold alone, both have no start to measure from.
        assert_eq!(seconds_since_last_start(&pool, "p").await.unwrap(), None);
        hold(&pool, "p", "main", "interval", "waiting")
            .await
            .unwrap();
        assert_eq!(seconds_since_last_start(&pool, "p").await.unwrap(), None);

        assert!(start(&pool, "p", "main", "a1", None).await.unwrap());
        let fresh = seconds_since_last_start(&pool, "p")
            .await
            .unwrap()
            .expect("a start records its time");
        assert!((0..30).contains(&fresh), "just started, got {fresh}s");

        sqlx::query(
            "UPDATE postgate_state SET last_started_at = datetime('now','-2 hours') \
             WHERE project_id = 'p'",
        )
        .execute(&pool)
        .await
        .unwrap();
        let aged = seconds_since_last_start(&pool, "p")
            .await
            .unwrap()
            .expect("an aged start still reads");
        assert!((7190..7300).contains(&aged), "two hours ago, got {aged}s");

        // Another project's start is not this one's.
        assert_eq!(
            seconds_since_last_start(&pool, "other").await.unwrap(),
            None
        );
    }

    /* --------------------------------------------- transitions (analytics F3) -- */

    /// `(to_state, sha, red_groups, reason, reverted)`.
    type TransitionRow = (String, Option<String>, String, Option<String>, i64);

    /// The transitions in the order they were written.
    async fn transitions(pool: &SqlitePool) -> Vec<TransitionRow> {
        sqlx::query_as(
            "SELECT to_state, sha, red_groups, reason, reverted \
             FROM postgate_transitions ORDER BY id",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// Spec §11: a red on a green project is one transition; a red on a red project is none.
    #[tokio::test]
    async fn finish_red_records_one_transition_only_when_the_project_was_green() {
        let pool = crate::testdb::fresh_pool().await;
        start(&pool, "p", "master", "a1", None).await.unwrap();
        assert!(
            finish_red(&pool, "p", "a1", &["core".to_owned()])
                .await
                .unwrap()
        );
        assert_eq!(
            transitions(&pool).await,
            vec![(
                "red".into(),
                Some("a1".into()),
                r#"["core"]"#.into(),
                Some("gate".into()),
                0
            )]
        );

        // Still red: the second red keeps the colour, so it records nothing.
        end_red(&pool, "a1").await;
        start(&pool, "p", "master", "b2", None).await.unwrap();
        assert!(
            finish_red(&pool, "p", "b2", &["shell".to_owned()])
                .await
                .unwrap()
        );
        assert_eq!(transitions(&pool).await.len(), 1);
    }

    /// A retried call is refused by the compare-and-set, so it cannot write a second row.
    #[tokio::test]
    async fn a_repeated_finish_red_does_not_duplicate_the_transition() {
        let pool = crate::testdb::fresh_pool().await;
        start(&pool, "p", "master", "a1", None).await.unwrap();
        assert!(
            finish_red(&pool, "p", "a1", &["core".to_owned()])
                .await
                .unwrap()
        );
        assert!(
            !finish_red(&pool, "p", "a1", &["core".to_owned()])
                .await
                .unwrap()
        );
        assert_eq!(transitions(&pool).await.len(), 1);
    }

    /// Spec §11: exactly one row on a real change, none on a refused (no-op) call.
    #[tokio::test]
    async fn finish_red_writes_exactly_one_transition_and_none_on_a_no_op() {
        let pool = crate::testdb::fresh_pool().await;
        // No row, wrong sha: refused, nothing written.
        assert!(
            !finish_red(&pool, "p", "zzz", &["core".to_owned()])
                .await
                .unwrap()
        );
        start(&pool, "p", "master", "a1", None).await.unwrap();
        assert!(
            !finish_red(&pool, "p", "zzz", &["core".to_owned()])
                .await
                .unwrap()
        );
        assert!(transitions(&pool).await.is_empty());
        assert!(
            finish_red(&pool, "p", "a1", &["core".to_owned()])
                .await
                .unwrap()
        );
        assert_eq!(transitions(&pool).await.len(), 1);
    }

    /// Spec §11: the transition and the state UPDATE commit together or not at all. A trigger
    /// makes the transition INSERT fail; the state must be exactly as before the call.
    #[tokio::test]
    async fn the_transition_and_the_state_update_commit_together() {
        let pool = crate::testdb::fresh_pool().await;
        start(&pool, "p", "master", "a1", None).await.unwrap();
        let before = load(&pool, "p").await.unwrap();
        sqlx::query(
            "CREATE TRIGGER refuse_transitions BEFORE INSERT ON postgate_transitions \
             BEGIN SELECT RAISE(ABORT, 'injected'); END",
        )
        .execute(&pool)
        .await
        .unwrap();

        assert!(
            finish_red(&pool, "p", "a1", &["core".to_owned()])
                .await
                .is_err()
        );
        assert_eq!(
            load(&pool, "p").await.unwrap(),
            before,
            "the UPDATE was rolled back"
        );
        assert!(transitions(&pool).await.is_empty());
    }

    /// Spec §11: each of the three green writes closes an open red with one `→ green` row.
    #[tokio::test]
    async fn finish_green_flake_green_and_mark_covered_each_close_an_open_red() {
        // finish_green
        let pool = crate::testdb::fresh_pool().await;
        start(&pool, "p", "master", "a1", None).await.unwrap();
        finish_red(&pool, "p", "a1", &["core".to_owned()])
            .await
            .unwrap();
        end_red(&pool, "a1").await;
        start(&pool, "p", "master", "b2", None).await.unwrap();
        assert!(finish_green(&pool, "p", "b2").await.unwrap());
        let rows = transitions(&pool).await;
        assert_eq!(rows.len(), 2);
        assert_eq!(
            rows[1],
            (
                "green".into(),
                Some("b2".into()),
                "[]".into(),
                Some("gate".into()),
                0
            )
        );

        // flake_green
        let pool = crate::testdb::fresh_pool().await;
        red_project(&pool, "g0", "r1").await;
        assert!(flake_green(&pool, "p", "r1").await.unwrap());
        let rows = transitions(&pool).await;
        assert_eq!(rows.last().unwrap().0, "green");
        assert_eq!(rows.last().unwrap().3.as_deref(), Some("flake"));

        // mark_covered on a red project
        let pool = crate::testdb::fresh_pool().await;
        red_project(&pool, "g0", "r1").await;
        mark_covered(&pool, "p", "master", "c9").await.unwrap();
        let rows = transitions(&pool).await;
        assert_eq!(rows.last().unwrap().0, "green");
        assert_eq!(rows.last().unwrap().1.as_deref(), Some("c9"));
        assert_eq!(rows.last().unwrap().3.as_deref(), Some("covered"));
    }

    #[tokio::test]
    async fn finish_green_writes_exactly_one_transition_and_none_on_a_no_op() {
        let pool = crate::testdb::fresh_pool().await;
        // Green to green: a passing gate on a green project changes no colour.
        start(&pool, "p", "master", "a1", None).await.unwrap();
        assert!(finish_green(&pool, "p", "a1").await.unwrap());
        // Refused: nothing running.
        assert!(!finish_green(&pool, "p", "a1").await.unwrap());
        assert!(transitions(&pool).await.is_empty());
    }

    #[tokio::test]
    async fn flake_green_writes_exactly_one_transition_and_none_on_a_no_op() {
        let pool = crate::testdb::fresh_pool().await;
        red_project(&pool, "g0", "r1").await; // one `→ red`
        assert!(!flake_green(&pool, "p", "zzz").await.unwrap());
        assert_eq!(transitions(&pool).await.len(), 1);
        assert!(flake_green(&pool, "p", "r1").await.unwrap());
        assert!(!flake_green(&pool, "p", "r1").await.unwrap());
        assert_eq!(transitions(&pool).await.len(), 2);
    }

    /// Spec §12.2 (conservative): a cover is `→ green` only when there was a red.
    #[tokio::test]
    async fn mark_covered_writes_exactly_one_transition_and_none_on_a_no_op() {
        let pool = crate::testdb::fresh_pool().await;
        mark_covered(&pool, "p", "master", "c1").await.unwrap(); // inserts a green row
        mark_covered(&pool, "p", "master", "c2").await.unwrap(); // green to green
        assert!(transitions(&pool).await.is_empty());
        red_project(&pool, "c2", "r1").await;
        mark_covered(&pool, "p", "master", "c3").await.unwrap();
        mark_covered(&pool, "p", "master", "c4").await.unwrap();
        let colours: Vec<String> = transitions(&pool)
            .await
            .into_iter()
            .map(|row| row.0)
            .collect();
        assert_eq!(colours, ["red", "green"]);
    }

    /// Spec §5.4: `settle_covered` never moves the colour, so it never writes a transition.
    #[tokio::test]
    async fn settle_covered_never_records_a_transition() {
        let pool = crate::testdb::fresh_pool().await;
        mark_covered(&pool, "p", "main", "g").await.unwrap();
        assert!(
            settle_covered(&pool, "p", "main", "c1", "g", "covered c1")
                .await
                .unwrap()
        );
        assert!(transitions(&pool).await.is_empty());
    }

    /// The green that follows a published revert of the red's culprit is marked `reverted`.
    #[tokio::test]
    async fn the_green_after_a_published_revert_is_marked_reverted() {
        let pool = crate::testdb::fresh_pool().await;
        red_project(&pool, "g0", "r1").await;
        // The state a published revert leaves: culprit known, revert started and committed for it.
        sqlx::query(
            "UPDATE postgate_state SET culprit_sha = 'm1', revert_merge_sha = 'm1', \
             revert_sha = 'v1', red_phase = NULL WHERE project_id = 'p'",
        )
        .execute(&pool)
        .await
        .unwrap();
        start(&pool, "p", "master", "v1", None).await.unwrap();
        assert!(finish_green(&pool, "p", "v1").await.unwrap());
        assert_eq!(transitions(&pool).await.last().unwrap().4, 1);
    }
}
