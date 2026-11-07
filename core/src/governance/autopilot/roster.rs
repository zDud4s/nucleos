//! The project roster the shell polls: one `ProjectSummary` per autopilot row, with its pending
//! work, shadow-exit readiness, WIP brake and last gate verdict.

use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;

use super::Mode;
use crate::wip::{open_proposals_term, open_shadow_decisions_term};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub project_id: String,
    pub mode: Mode,
    pub project_root: Option<String>,
    pub pending: i64,
    /// Shadow-exit progress (§8.2): action classes clearing the bar, out of those exercised so far.
    pub classes_ready: i64,
    pub classes_total: i64,
    /// How many of those ready classes are ones the classifier WITHHELD (`pending_approval`/`deny`).
    /// Surfaced separately because it is the criterion a project can fail while looking finished:
    /// every class ready out of every class exercised, and still nothing proving restraint.
    pub withheld_classes_ready: i64,
    /// Whether the shell should unlock the promote-to-active control. Computed here rather than in
    /// the shell so the displayed gate and the enforced rule are the same arithmetic (`shadow.rs`).
    pub promotable: bool,
    /// WIP brake (§8.4): proposals waiting on the human, the effective ceiling (`None` = brake off),
    /// and whether the project is currently deferring new work because of it. A project can be idle
    /// purely because its queue is full, so the UI has to be able to say so.
    pub open_review_items: i64,
    /// The two queues `open_review_items` is the sum of, so a screen can say WHICH one is full.
    ///
    /// The total on its own is what produced the report this field exists to answer: the panel said
    /// the queue was full, the proposals list was empty, and both were telling the truth about
    /// different queues. Measured on `nucleos` 2026-09-08 -- 0 proposals, 13 shadow decisions --
    /// which is exactly the shape that makes a correct total look like a broken one.
    ///
    /// Summed in Rust rather than in SQL so the parts and the whole cannot disagree: there is no
    /// second expression here that could be changed without the first.
    pub open_proposals: i64,
    pub open_shadow_decisions: i64,
    pub wip_limit: Option<i64>,
    pub queue_full: bool,
    /// The last thing this project's gate said, and when it said it.
    ///
    /// The LAST verdict and not a tally over a window, because the roster's question is *is this
    /// one broken right now*. [`crate::project::readings`] already answers "how has it been going"
    /// for the one project somebody opened, and a thirty-day count in a roster column would read
    /// green for a project that broke this morning.
    ///
    /// `None` means this project has never produced one — no gate command, or no run that got far
    /// enough to reach it. `0032_run_gate.sql` argues that case: it is not a pass and not a
    /// failure, it is nobody having asked for a measurement.
    pub last_gate: Option<String>,
    pub last_gate_at: Option<String>,
    /// The folder's `origin` URL, so another machine can tell it holds the same repository. Not read
    /// by the roster query: `GET /projects` fills it in (`http/projects.rs`). `None` is a folder with
    /// no `origin`, no git, or a read that failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote: Option<String>,
}

/// One raw roster row: `(project_id, mode, project_root, pending, open_proposals,
/// open_shadow_decisions, wip_override)`. Named because the tuple carries seven positional fields
/// and is only readable at the destructure.
type RosterRow = (String, String, Option<String>, i64, i64, i64, Option<i64>);

/// The most recent gate verdict for every project, in one query.
///
/// One grouped read and not one per row, for the reason `shadow_readiness` is one: the shell polls
/// the roster every three seconds, and a call per project turns a roster of twenty-five into
/// twenty-five round trips on every tick.
///
/// **This leans on a documented SQLite behaviour and says so, because it is not standard SQL.** With
/// a bare column beside `MAX(...)` in an aggregate query, SQLite takes that column from the row
/// that supplied the maximum — so `gate_status` here is the status of the newest run, not an
/// arbitrary one from the group. Every other engine is free to return any row, and this query would
/// have to be rewritten as a window function the day this stops being SQLite.
///
/// Rows with no verdict are excluded rather than counted as anything: a run that never reached its
/// gate says nothing about the code, and the last run that DID reach one is still the answer.
async fn last_gate_verdicts(
    pool: &SqlitePool,
) -> sqlx::Result<std::collections::HashMap<String, (String, String)>> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT project_id, gate_status, MAX(completed_at)
         FROM runs
         WHERE project_id IS NOT NULL
           AND gate_status IS NOT NULL
           AND completed_at IS NOT NULL
         GROUP BY project_id",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|(project_id, status, at)| (project_id, (status, at)))
        .collect())
}

pub async fn project_roster(pool: &SqlitePool) -> sqlx::Result<Vec<ProjectSummary>> {
    // Every subquery below that also feeds the WIP brake is EXPANDED FROM `wip.rs`, not retyped.
    // These were two hand-written copies of one piece of arithmetic, and they drifted twice --
    // most recently into a roster that displayed 19 against a daemon that refused work at 13. The
    // macro takes the only thing that differs (a bound `?1` there, a correlated `state.project_id`
    // here) as an argument, so the filters cannot be changed in one place and not the other.
    //
    // Selected as two columns and added in Rust, rather than summed in SQL: the parts are what the
    // shell needs to point at a screen, and deriving the total from them is one fewer expression
    // that could go out of step with itself.
    // The shadow-decisions term is computed ONCE per row, in a MATERIALIZED CTE, and `pending` /
    // `open_shadow_decisions` are derived from that column. Written as a plain subquery SQLite
    // flattens it and pastes the expression back into both places (measured with EXPLAIN QUERY
    // PLAN: the term ran twice per row), and that term is the expensive one.
    let projects: Vec<RosterRow> = sqlx::query_as(concat!(
        "WITH s AS MATERIALIZED (
            SELECT state.project_id AS project_id, state.mode AS mode,
                   state.project_root AS project_root,
                   ",
        open_shadow_decisions_term!("state.project_id"),
        " AS open_shadow_decisions,
                   (SELECT COUNT(*)
                    FROM runs
                    WHERE runs.project_id = state.project_id
                      AND runs.status = 'awaiting_approval') AS awaiting,
                   ",
        open_proposals_term!("state.project_id"),
        " AS open_proposals,
                   state.wip_limit AS wip_limit
            FROM autopilot_state AS state)
         SELECT project_id, mode, project_root,
                open_shadow_decisions + awaiting AS pending,
                open_proposals, open_shadow_decisions, wip_limit
         FROM s
         ORDER BY project_id"
    ))
    .fetch_all(pool)
    .await?;

    // One grouped query for every project's readiness, rather than a scoreboard call per row — the
    // shell polls this endpoint every 3s. Same reason the WIP ceiling is resolved from one global
    // read plus the per-row override already selected above.
    let readiness = crate::governance::shadow::shadow_readiness(pool).await?;
    let global_wip_limit = crate::wip::global_wip_limit(pool).await?;
    let gates = last_gate_verdicts(pool).await?;

    projects
        .into_iter()
        .map(
            |(
                project_id,
                mode,
                project_root,
                pending,
                open_proposals,
                open_shadow_decisions,
                wip_override,
            )| {
                let mode = Mode::from_db_str(&mode).ok_or_else(|| {
                    sqlx::Error::Protocol(format!("invalid autopilot mode in database: {mode}"))
                })?;
                let (classes_ready, classes_total, withheld_classes_ready) =
                    readiness.get(&project_id).copied().unwrap_or((0, 0, 0));
                let wip_limit = wip_override.or(global_wip_limit);
                let (last_gate, last_gate_at) = match gates.get(&project_id) {
                    Some((status, at)) => (Some(status.clone()), Some(at.clone())),
                    None => (None, None),
                };
                let open_review_items = open_proposals + open_shadow_decisions;
                Ok(ProjectSummary {
                    project_id,
                    mode,
                    project_root,
                    pending,
                    classes_ready,
                    classes_total,
                    withheld_classes_ready,
                    promotable: crate::governance::shadow::promotable(
                        classes_ready,
                        classes_total,
                        withheld_classes_ready,
                    ),
                    open_review_items,
                    open_proposals,
                    open_shadow_decisions,
                    wip_limit,
                    queue_full: crate::wip::queue_full(open_review_items, wip_limit),
                    last_gate,
                    last_gate_at,
                    remote: None,
                })
            },
        )
        .collect()
}
