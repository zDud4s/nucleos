//! Automatic launch of a ticket nobody took (spec 2026-11-06-tickets-globais, "automatic launch, cap
//! 2, then parking").
//!
//! Every minute `run_launch_loop` runs [`tick`]: first the sweep (`store::reopen_lapsed`, so a claim
//! whose holder died counts as free), then, for every `open` ticket that is not parked, the pure
//! [`due`]. A ticket that stayed unowned for ten minutes (or that every competent holder declined) gets
//! a run; a key that already had two launches in seven days is parked instead: `parked_at` is set and
//! the owner is asked, through an ownerless decision, to pick an owner or close it. The cap counts
//! every row of the key, closed ones included, so a problem that keeps coming back is not relaunched
//! forever. `master-red` and `migration:` tickets have their own owners and are never touched here.
//!
//! A launch is a single flight on `launch_attempts`: the attempt is counted before the launcher runs,
//! and a launch that fails still counts.

use std::collections::BTreeSet;
use std::time::Duration as StdDuration;

use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use serde_json::{Value, json};
use sqlx::SqlitePool;

use super::competence;
use super::store;
use super::{MASTER_RED_KEY, Status, TaskRow};
use crate::execution::wave::stamp;
use crate::state::AppState;

/// How long an `open` ticket waits for an owner before the daemon launches a run for it.
pub const LAUNCH_AFTER_SECONDS: i64 = 600;
/// Launches per key in [`HISTORY_DAYS`]; past it the ticket is parked into a decision.
pub const MAX_AUTO_LAUNCHES: i64 = 2;
/// The window the launch cap counts over.
const HISTORY_DAYS: i64 = 7;
/// How often the loop looks at the tickets.
const LOOP_INTERVAL: StdDuration = StdDuration::from_secs(60);
/// Keys whose owners are fixed elsewhere: the land gate's migration tickets.
const MIGRATION_PREFIX: &str = "migration:";
/// Keys that belong to a spec's own tree.
const SPEC_ITEM_PREFIX: &str = "spec-item:";

/// What a launch asks for: one branch with no sub-nodes, or a full tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchKind {
    Leaf,
    Tree,
}

/// The launches of a key over the last [`HISTORY_DAYS`], every row of the key counted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyHistory {
    pub launches_7d: i64,
    pub last_launch_at: Option<String>,
}

/// What the loop does with one ticket on this tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    No,
    Launch,
    Park,
}

/// Starts a run for a ticket. `attempt` is the ticket's attempt number, from 1; the returned id names
/// what was started (the tree id) and goes into the `launched` event.
#[async_trait]
pub trait TicketLauncher: Send + Sync {
    async fn launch(
        &self,
        task: &TaskRow,
        kind: LaunchKind,
        attempt: i64,
    ) -> Result<String, String>;
}

/// The package a path belongs to: its first segment, or its first two under `sidecars/`.
fn package_of(path: &str) -> &str {
    let path = path.trim_start_matches('/');
    let mut segments = path.splitn(3, '/');
    let first = segments.next().unwrap_or("");
    if first == "sidecars"
        && let Some(second) = segments.next()
    {
        return &path[..first.len() + 1 + second.len()];
    }
    first
}

/// `Leaf` when the ticket's paths span at most one package and it carries no spec; `Tree` otherwise.
pub fn launch_kind(task: &TaskRow) -> LaunchKind {
    let has_spec = !matches!(task.detail.get("spec"), None | Some(Value::Null));
    if has_spec || task.key.as_str().starts_with(SPEC_ITEM_PREFIX) {
        return LaunchKind::Tree;
    }
    let packages: BTreeSet<&str> = task
        .guard_paths
        .iter()
        .chain(&task.hint_paths)
        .map(|path| package_of(path))
        .collect();
    if packages.len() <= 1 {
        LaunchKind::Leaf
    } else {
        LaunchKind::Tree
    }
}

/// False for the keys whose owners are decided elsewhere: `master-red` (verify and the postgate) and
/// `migration:` (the branch the land gate refused).
pub fn auto_launchable(key: &str) -> bool {
    key != MASTER_RED_KEY && !key.starts_with(MIGRATION_PREFIX)
}

fn parse(at: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(at)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

/// Whether `at` is at least [`LAUNCH_AFTER_SECONDS`] before `now`. An unreadable stamp never is.
fn quiet_since(at: Option<&str>, now: DateTime<Utc>) -> bool {
    at.and_then(parse)
        .is_some_and(|at| now - at >= Duration::seconds(LAUNCH_AFTER_SECONDS))
}

/// What to do with `task` now. Only an `open`, unparked ticket of a launchable key is ever due: it
/// launches while its key has had fewer than [`MAX_AUTO_LAUNCHES`] launches, after ten quiet minutes
/// (since the last launch, or since it was created) or at once when every competent holder declined;
/// past the cap it parks, ten minutes after the last launch.
pub fn due(task: &TaskRow, history: &KeyHistory, all_declined: bool, now: DateTime<Utc>) -> Due {
    if !auto_launchable(task.key.as_str())
        || task.status != Status::Open
        || task.parked_at.is_some()
    {
        return Due::No;
    }
    if history.launches_7d >= MAX_AUTO_LAUNCHES {
        return if quiet_since(history.last_launch_at.as_deref(), now) {
            Due::Park
        } else {
            Due::No
        };
    }
    let since = history
        .last_launch_at
        .as_deref()
        .unwrap_or(task.created_at.as_str());
    if all_declined || quiet_since(Some(since), now) {
        Due::Launch
    } else {
        Due::No
    }
}

async fn key_history(
    pool: &SqlitePool,
    task: &TaskRow,
    now: DateTime<Utc>,
) -> sqlx::Result<KeyHistory> {
    let (launches_7d, last_launch_at): (i64, Option<String>) = sqlx::query_as(
        "SELECT COALESCE(SUM(launch_attempts), 0), MAX(last_launch_at) FROM task_claims
         WHERE project_id = ? AND task_key = ? AND COALESCE(last_launch_at, created_at) >= ?",
    )
    .bind(&task.project_id)
    .bind(task.key.as_str())
    .bind(stamp(now - Duration::days(HISTORY_DAYS)))
    .fetch_one(pool)
    .await?;
    Ok(KeyHistory {
        launches_7d,
        last_launch_at,
    })
}

/// What `offers::route` would answer `AllDeclined` to, read without writing anything.
async fn all_declined(pool: &SqlitePool, task: &TaskRow, now: DateTime<Utc>) -> sqlx::Result<bool> {
    let found = competence::candidates(pool, task, now).await?;
    if found.is_empty() {
        return Ok(false);
    }
    let declined = competence::declined_set(pool, task.id).await?;
    Ok(competence::rank(found, &declined).is_empty())
}

async fn load(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<TaskRow>> {
    let mut conn = pool.acquire().await?;
    store::row_by_id(&mut conn, id).await
}

/// One pass: the sweep, then every `open`, unparked ticket. A ticket whose step fails is logged and
/// the others still run.
pub async fn tick(
    pool: &SqlitePool,
    launcher: &dyn TicketLauncher,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    store::reopen_lapsed(pool, now).await?;
    let ids: Vec<i64> = sqlx::query_scalar(
        "SELECT id FROM task_claims WHERE status = 'open' AND parked_at IS NULL ORDER BY id",
    )
    .fetch_all(pool)
    .await?;
    for id in ids {
        if let Err(error) = step(pool, launcher, id, now).await {
            tracing::warn!(%error, task_id = id, "ticket launch step failed");
        }
    }
    Ok(())
}

async fn step(
    pool: &SqlitePool,
    launcher: &dyn TicketLauncher,
    id: i64,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    let Some(task) = load(pool, id).await? else {
        return Ok(());
    };
    if !auto_launchable(task.key.as_str()) {
        return Ok(());
    }
    let history = key_history(pool, &task, now).await?;
    let declined = all_declined(pool, &task, now).await?;
    match due(&task, &history, declined, now) {
        Due::No => Ok(()),
        Due::Launch => launch(pool, launcher, &task, now).await,
        Due::Park => park(pool, &task, now).await,
    }
}

/// Counts the attempt first (compare-and-swap on the count read with the row: a second tick that read
/// the same count changes nothing and launches nothing), then launches and records `launched`.
async fn launch(
    pool: &SqlitePool,
    launcher: &dyn TicketLauncher,
    task: &TaskRow,
    now: DateTime<Utc>,
) -> sqlx::Result<()> {
    let won = sqlx::query(
        "UPDATE task_claims SET launch_attempts = launch_attempts + 1, last_launch_at = ?
         WHERE id = ? AND launch_attempts = ? AND status = 'open'",
    )
    .bind(stamp(now))
    .bind(task.id)
    .bind(task.launch_attempts)
    .execute(pool)
    .await?
    .rows_affected();
    if won == 0 {
        return Ok(());
    }
    let attempt = task.launch_attempts + 1;
    let note = match launcher.launch(task, launch_kind(task), attempt).await {
        Ok(launched) => launched,
        Err(error) => {
            tracing::warn!(%error, task_id = task.id, attempt, "ticket launch failed");
            format!("launch failed: {error}")
        }
    };
    store::record_event(pool, task.id, "launched", None, &note, now).await
}

/// Parks the ticket (single flight on `parked_at`), records `parked` and asks the owner. The decision
/// is fail-open: the ticket stays parked and the error is logged.
async fn park(pool: &SqlitePool, task: &TaskRow, now: DateTime<Utc>) -> sqlx::Result<()> {
    let won =
        sqlx::query("UPDATE task_claims SET parked_at = ? WHERE id = ? AND parked_at IS NULL")
            .bind(stamp(now))
            .bind(task.id)
            .execute(pool)
            .await?
            .rows_affected();
    if won == 0 {
        return Ok(());
    }
    let note = format!("{MAX_AUTO_LAUNCHES} launches left it unowned");
    store::record_event(pool, task.id, "parked", None, &note, now).await?;
    let question = format!(
        "Ticket #{} {} {} was launched twice and nobody fixed it. Pick an owner or close it.",
        task.id,
        task.key.as_str(),
        task.title
    );
    let decision_id = format!("ticket-{}", task.id);
    if let Err(error) =
        crate::tree_job::decisions::ask_ownerless(pool, &decision_id, &question).await
    {
        tracing::warn!(%error, task_id = task.id, "could not ask the owner about a parked ticket");
    }
    Ok(())
}

/// Every stored report of the ticket: its text and its paths.
async fn reports_of(pool: &SqlitePool, task_id: i64) -> sqlx::Result<Vec<(String, Vec<String>)>> {
    let rows: Vec<(String, String)> = sqlx::query_as(
        "SELECT detail, paths FROM task_claim_reports WHERE task_id = ? ORDER BY id",
    )
    .bind(task_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(detail, paths)| {
            let paths = serde_json::from_str::<Vec<String>>(&paths).unwrap_or_default();
            (detail, paths)
        })
        .collect())
}

/// The goal of the launched tree: the title, every report, the paths and the claim to take first.
fn goal_of(task: &TaskRow, reports: &[(String, Vec<String>)], kind: LaunchKind) -> String {
    let mut goal = format!("Fix ticket #{}: {}\n", task.id, task.title);
    if !reports.is_empty() {
        goal.push_str("\nReports:\n");
        for (detail, paths) in reports {
            goal.push_str(&format!("- {detail}"));
            if !paths.is_empty() {
                goal.push_str(&format!(" (paths: {})", paths.join(", ")));
            }
            goal.push('\n');
        }
    }
    let paths: BTreeSet<&str> = task
        .guard_paths
        .iter()
        .chain(&task.hint_paths)
        .map(String::as_str)
        .collect();
    if !paths.is_empty() {
        let paths: Vec<&str> = paths.into_iter().collect();
        goal.push_str(&format!("\nPaths: {}\n", paths.join(", ")));
    }
    if kind == LaunchKind::Leaf {
        goal.push_str("\nKeep it to one branch, no sub-nodes.\n");
    }
    goal.push_str(&format!(
        "\nClaim `{}` with task_claim before working.\n",
        task.key.as_str()
    ));
    goal
}

/// The daemon's launcher: `POST /trees` through the in-process tree router, as `spec_backlog` does.
/// The tree id is `ticket-<id>-<attempt>`, so a repeated attempt answers with the stored job.
pub struct DaemonTicketLauncher {
    pub state: AppState,
}

#[async_trait]
impl TicketLauncher for DaemonTicketLauncher {
    async fn launch(
        &self,
        task: &TaskRow,
        kind: LaunchKind,
        attempt: i64,
    ) -> Result<String, String> {
        let pool = &self.state.pool;
        let root = crate::governance::autopilot::root_of(pool, &task.project_id)
            .await
            .map_err(|error| error.to_string())?
            .ok_or_else(|| format!("project {} has no registered root", task.project_id))?;
        let reports = reports_of(pool, task.id)
            .await
            .map_err(|error| error.to_string())?;
        let id = format!("ticket-{}-{attempt}", task.id);
        let body = json!({
            "id": id,
            "goal": goal_of(task, &reports, kind),
            "feature_branch": format!("fix/ticket-{}", task.id),
            "project_root": root,
            "project_id": task.project_id,
        });
        post_tree(&self.state, body).await?;
        Ok(id)
    }
}

async fn post_tree(state: &AppState, body: Value) -> Result<(), String> {
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;
    let request = Request::builder()
        .method("POST")
        .uri("/trees")
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .map_err(|error| error.to_string())?;
    let response = crate::tree_job::http::routes()
        .with_state(state.clone())
        .oneshot(request)
        .await
        .map_err(|error| error.to_string())?;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1 << 20)
        .await
        .map_err(|error| error.to_string())?;
    if !status.is_success() {
        return Err(format!(
            "POST /trees answered {status}: {}",
            String::from_utf8_lossy(&bytes)
        ));
    }
    Ok(())
}

/// The daemon loop: one [`tick`] a minute. An error is logged and the next tick tries again.
pub async fn run_launch_loop(state: AppState) {
    let launcher = DaemonTicketLauncher {
        state: state.clone(),
    };
    let mut interval = tokio::time::interval(LOOP_INTERVAL);
    loop {
        interval.tick().await;
        if let Err(error) = tick(&state.pool, &launcher, Utc::now()).await {
            tracing::warn!(%error, "ticket launch tick failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_claims::{ClaimOutcome, Holder, HolderKind, TaskKind, testing};
    use chrono::TimeZone;
    use std::sync::Mutex;

    fn t0() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 7, 10, 0, 0).unwrap()
    }

    fn mins(n: i64) -> Duration {
        Duration::minutes(n)
    }

    fn never() -> KeyHistory {
        KeyHistory::default()
    }

    fn launched(count: i64, at: DateTime<Utc>) -> KeyHistory {
        KeyHistory {
            launches_7d: count,
            last_launch_at: Some(stamp(at)),
        }
    }

    /// An `open` ticket of `key` created at `created`.
    fn ticket_row(key: &str, created: DateTime<Utc>) -> TaskRow {
        let mut row = testing::row(key, 1);
        row.created_at = stamp(created);
        row
    }

    #[test]
    fn due_launches_after_ten_minutes_without_a_claim() {
        let row = ticket_row("file:core/a.rs", t0() - mins(10));
        assert_eq!(due(&row, &never(), false, t0()), Due::Launch);
        let young = ticket_row("file:core/a.rs", t0() - mins(9));
        assert_eq!(due(&young, &never(), false, t0()), Due::No);
        // After one launch the ten minutes count from that launch, not from the creation.
        assert_eq!(
            due(&row, &launched(1, t0() - mins(5)), false, t0()),
            Due::No
        );
        assert_eq!(
            due(&row, &launched(1, t0() - mins(10)), false, t0()),
            Due::Launch
        );
    }

    #[test]
    fn due_launches_at_once_when_every_candidate_declined() {
        let row = ticket_row("file:core/a.rs", t0() - mins(1));
        assert_eq!(due(&row, &never(), true, t0()), Due::Launch);
        assert_eq!(due(&row, &never(), false, t0()), Due::No);
        assert_eq!(
            due(&row, &launched(1, t0() - mins(1)), true, t0()),
            Due::Launch
        );
    }

    #[test]
    fn due_never_launches_a_claimed_or_parked_ticket() {
        let mut claimed = ticket_row("file:core/a.rs", t0() - mins(60));
        claimed.status = Status::Claimed;
        assert_eq!(due(&claimed, &never(), true, t0()), Due::No);
        let mut parked = ticket_row("file:core/a.rs", t0() - mins(60));
        parked.parked_at = Some(stamp(t0() - mins(30)));
        assert_eq!(due(&parked, &never(), true, t0()), Due::No);
        assert_eq!(
            due(&parked, &launched(2, t0() - mins(30)), false, t0()),
            Due::No
        );
        let mut done = ticket_row("file:core/a.rs", t0() - mins(60));
        done.status = Status::Done;
        assert_eq!(due(&done, &never(), true, t0()), Due::No);
    }

    #[test]
    fn due_parks_after_the_second_launch_left_the_ticket_unowned() {
        let row = ticket_row("file:core/a.rs", t0() - mins(60));
        assert_eq!(
            due(&row, &launched(2, t0() - mins(10)), false, t0()),
            Due::Park
        );
        assert_eq!(
            due(&row, &launched(2, t0() - mins(5)), false, t0()),
            Due::No
        );
        assert_eq!(
            due(&row, &launched(2, t0() - mins(5)), true, t0()),
            Due::No,
            "every candidate declining never buys a third launch"
        );
        assert_eq!(
            due(&row, &launched(3, t0() - mins(10)), true, t0()),
            Due::Park
        );
    }

    #[test]
    fn launch_kind_is_leaf_for_one_package_without_a_spec_and_tree_otherwise() {
        let with = |guard: &[&str], hint: &[&str]| {
            let mut row = testing::row("file:core/a.rs", 1);
            row.guard_paths = guard.iter().map(|p| p.to_string()).collect();
            row.hint_paths = hint.iter().map(|p| p.to_string()).collect();
            row
        };
        assert_eq!(launch_kind(&with(&[], &[])), LaunchKind::Leaf);
        assert_eq!(
            launch_kind(&with(&["core/src/a.rs", "core/src/b/c.rs"], &[])),
            LaunchKind::Leaf
        );
        assert_eq!(
            launch_kind(&with(&["core/src/a.rs"], &["shell/src/b.ts"])),
            LaunchKind::Tree
        );
        assert_eq!(
            launch_kind(&with(
                &["sidecars/email/a.go", "sidecars/email/x/b.go"],
                &[]
            )),
            LaunchKind::Leaf
        );
        assert_eq!(
            launch_kind(&with(&["sidecars/email/a.go", "sidecars/web/b.go"], &[])),
            LaunchKind::Tree
        );
        let mut spec = with(&["core/src/a.rs"], &[]);
        spec.detail = json!({ "spec": "2026-11-06-tickets-globais" });
        assert_eq!(launch_kind(&spec), LaunchKind::Tree);
        let mut item = with(&["core/src/a.rs"], &[]);
        item.key = crate::task_claims::TaskKey("spec-item:x".to_owned());
        assert_eq!(launch_kind(&item), LaunchKind::Tree);
    }

    #[test]
    fn a_master_red_ticket_is_never_launched_or_parked_by_the_loop() {
        assert!(!auto_launchable(MASTER_RED_KEY));
        let row = ticket_row(MASTER_RED_KEY, t0() - Duration::days(1));
        assert_eq!(due(&row, &never(), true, t0()), Due::No);
        assert_eq!(
            due(&row, &launched(2, t0() - mins(60)), true, t0()),
            Due::No
        );
    }

    #[test]
    fn a_migration_ticket_is_never_launched_or_parked_by_the_loop() {
        let key = "migration:master:0286";
        assert!(!auto_launchable(key));
        assert!(auto_launchable("file:core/a.rs"));
        let row = ticket_row(key, t0() - Duration::days(1));
        assert_eq!(due(&row, &never(), true, t0()), Due::No);
        assert_eq!(
            due(&row, &launched(2, t0() - mins(60)), true, t0()),
            Due::No
        );
    }

    // ---- tick ---------------------------------------------------------------------------------

    /// Records every launch; answers `Err` when `fail` is set.
    #[derive(Default)]
    struct FakeLauncher {
        calls: Mutex<Vec<(i64, LaunchKind, i64)>>,
        fail: bool,
    }

    impl FakeLauncher {
        fn failing() -> Self {
            Self {
                fail: true,
                ..Self::default()
            }
        }

        fn calls(&self) -> Vec<(i64, LaunchKind, i64)> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl TicketLauncher for FakeLauncher {
        async fn launch(
            &self,
            task: &TaskRow,
            kind: LaunchKind,
            attempt: i64,
        ) -> Result<String, String> {
            self.calls.lock().unwrap().push((task.id, kind, attempt));
            if self.fail {
                Err("no room".to_owned())
            } else {
                Ok(format!("fake-{}-{attempt}", task.id))
            }
        }
    }

    /// An `open` declared ticket of project `alpha` guarding `core/a.rs`, created at `created`.
    async fn open_ticket(pool: &SqlitePool, key: &str, created: DateTime<Utc>) -> i64 {
        let mut new = testing::new_task(key, TaskKind::Declared);
        new.guard_paths = vec!["core/a.rs".to_owned()];
        store::create_or_adopt_open(pool, "alpha", new, created)
            .await
            .unwrap()
            .id
    }

    async fn task(pool: &SqlitePool, id: i64) -> TaskRow {
        load(pool, id).await.unwrap().unwrap()
    }

    /// `(event, holder_id, note)` of the ticket's events after `created`.
    async fn events(pool: &SqlitePool, id: i64) -> Vec<(String, Option<String>, String)> {
        sqlx::query_as(
            "SELECT event, holder_id, note FROM task_claim_events
             WHERE task_id = ? AND event != 'created' ORDER BY id",
        )
        .bind(id)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    /// Closes `id` as `done` with `attempts` launches, the last at `last`.
    async fn close_launched(pool: &SqlitePool, id: i64, attempts: i64, last: DateTime<Utc>) {
        sqlx::query(
            "UPDATE task_claims SET status = 'done', closed_at = ?, launch_attempts = ?, last_launch_at = ?
             WHERE id = ?",
        )
        .bind(stamp(last))
        .bind(attempts)
        .bind(stamp(last))
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn insert_run(pool: &SqlitePool, status: &str) -> i64 {
        sqlx::query("INSERT INTO runs (prompt, status, created_at) VALUES ('x', ?, 't')")
            .bind(status)
            .execute(pool)
            .await
            .unwrap()
            .last_insert_rowid()
    }

    async fn claim_as(pool: &SqlitePool, holder: &Holder, key: &str, at: DateTime<Utc>) -> i64 {
        let new = testing::new_task(key, TaskKind::Declared);
        match store::claim(pool, "alpha", new, holder, at).await.unwrap() {
            ClaimOutcome::Won { task, .. } => task.id,
            ClaimOutcome::Lost { .. } => panic!("{key} was already held"),
        }
    }

    #[tokio::test]
    async fn a_due_ticket_is_launched_once_and_recorded_with_a_launched_event() {
        let pool = crate::testdb::fresh_pool().await;
        let id = open_ticket(&pool, "file:core/a.rs", t0() - mins(11)).await;
        let launcher = FakeLauncher::default();
        tick(&pool, &launcher, t0()).await.unwrap();
        tick(&pool, &launcher, t0() + mins(1)).await.unwrap();
        assert_eq!(launcher.calls(), [(id, LaunchKind::Leaf, 1)]);
        let row = task(&pool, id).await;
        assert_eq!(row.launch_attempts, 1);
        assert_eq!(row.last_launch_at, Some(stamp(t0())));
        assert_eq!(row.status, Status::Open);
        assert_eq!(
            events(&pool, id).await,
            [("launched".to_owned(), None, format!("fake-{id}-1"))]
        );
    }

    #[tokio::test]
    async fn two_ticks_in_the_same_second_launch_once() {
        let pool = crate::testdb::fresh_pool().await;
        let id = open_ticket(&pool, "file:core/a.rs", t0() - mins(11)).await;
        let launcher = FakeLauncher::default();
        let (first, second) =
            tokio::join!(tick(&pool, &launcher, t0()), tick(&pool, &launcher, t0()));
        first.unwrap();
        second.unwrap();
        tick(&pool, &launcher, t0()).await.unwrap();
        assert_eq!(launcher.calls(), [(id, LaunchKind::Leaf, 1)]);
        assert_eq!(task(&pool, id).await.launch_attempts, 1);
        assert_eq!(events(&pool, id).await.len(), 1);
    }

    #[tokio::test]
    async fn a_ticket_that_is_no_longer_open_is_not_launched() {
        let pool = crate::testdb::fresh_pool().await;
        let id = open_ticket(&pool, "file:core/a.rs", t0() - mins(11)).await;
        // The pass read the row while it was open; the ticket then closed before the launch.
        let stale = task(&pool, id).await;
        close_launched(&pool, id, 0, t0() - mins(11)).await;
        let launcher = FakeLauncher::default();
        launch(&pool, &launcher, &stale, t0()).await.unwrap();
        assert!(launcher.calls().is_empty());
        assert_eq!(task(&pool, id).await.launch_attempts, 0);
        assert!(events(&pool, id).await.is_empty());
    }

    #[tokio::test]
    async fn a_failed_launch_counts_as_an_attempt() {
        let pool = crate::testdb::fresh_pool().await;
        let id = open_ticket(&pool, "file:core/a.rs", t0() - mins(11)).await;
        let launcher = FakeLauncher::failing();
        tick(&pool, &launcher, t0()).await.unwrap();
        assert_eq!(launcher.calls().len(), 1);
        let row = task(&pool, id).await;
        assert_eq!(row.launch_attempts, 1);
        assert_eq!(row.last_launch_at, Some(stamp(t0())));
        let log = events(&pool, id).await;
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].0, "launched");
        assert!(
            log[0].2.contains("no room"),
            "the note carries the error: {}",
            log[0].2
        );
    }

    #[tokio::test]
    async fn the_third_decision_is_parking_not_a_third_launch() {
        let pool = crate::testdb::fresh_pool().await;
        let id = open_ticket(&pool, "file:core/a.rs", t0() - mins(11)).await;
        let launcher = FakeLauncher::default();
        tick(&pool, &launcher, t0()).await.unwrap();
        tick(&pool, &launcher, t0() + mins(10)).await.unwrap();
        tick(&pool, &launcher, t0() + mins(20)).await.unwrap();
        tick(&pool, &launcher, t0() + mins(40)).await.unwrap();
        assert_eq!(
            launcher.calls(),
            [(id, LaunchKind::Leaf, 1), (id, LaunchKind::Leaf, 2)]
        );
        let row = task(&pool, id).await;
        assert_eq!(row.launch_attempts, 2);
        assert_eq!(row.parked_at, Some(stamp(t0() + mins(20))));
        assert_eq!(row.status, Status::Open, "a parked ticket stays claimable");
        let names: Vec<String> = events(&pool, id).await.into_iter().map(|e| e.0).collect();
        assert_eq!(names, ["launched", "launched", "parked"]);
    }

    #[tokio::test]
    async fn attempts_survive_a_restart() {
        let pool = crate::testdb::fresh_pool().await;
        let id = open_ticket(&pool, "file:core/a.rs", t0() - mins(11)).await;
        let before = FakeLauncher::default();
        tick(&pool, &before, t0()).await.unwrap();
        // A restarted daemon: a new launcher and a new tick over the same database.
        let after = FakeLauncher::default();
        tick(&pool, &after, t0() + mins(10)).await.unwrap();
        assert_eq!(after.calls(), [(id, LaunchKind::Leaf, 2)]);
        let again = FakeLauncher::default();
        tick(&pool, &again, t0() + mins(20)).await.unwrap();
        assert!(
            again.calls().is_empty(),
            "the cap counts launches made before the restart"
        );
        assert!(task(&pool, id).await.parked_at.is_some());
    }

    #[tokio::test]
    async fn a_session_that_died_holding_the_claim_gets_its_ticket_launched() {
        let pool = crate::testdb::fresh_pool().await;
        let holder = Holder {
            kind: HolderKind::Session,
            id: "s1".to_owned(),
            worktree: "/wt/s1".to_owned(),
        };
        // Claimed two hours ago: its 30-minute lease lapsed long before `t0`.
        let id = claim_as(&pool, &holder, "file:core/a.rs", t0() - mins(120)).await;
        let launcher = FakeLauncher::default();
        tick(&pool, &launcher, t0()).await.unwrap();
        assert_eq!(launcher.calls().len(), 1);
        assert_eq!(launcher.calls()[0].0, id);
        let log = events(&pool, id).await;
        let reopened = log
            .iter()
            .position(|e| e.0 == "expired_reclaimed")
            .expect("the sweep reopened the claim");
        assert_eq!(log[reopened].1.as_deref(), Some("s1"));
        assert_eq!(log[reopened].2, "lease_lapsed");
        assert_eq!(log.last().unwrap().0, "launched");
    }

    #[tokio::test]
    async fn a_run_claim_whose_run_ended_gets_reopened_by_the_sweep() {
        let pool = crate::testdb::fresh_pool().await;
        let run = insert_run(&pool, "completed").await;
        let holder = Holder {
            kind: HolderKind::Run,
            id: run.to_string(),
            worktree: "/wt/run".to_owned(),
        };
        let id = claim_as(&pool, &holder, "file:core/a.rs", t0() - mins(30)).await;
        let launcher = FakeLauncher::default();
        tick(&pool, &launcher, t0()).await.unwrap();
        let row = task(&pool, id).await;
        assert_eq!(row.holder(), None);
        let log = events(&pool, id).await;
        assert!(log.iter().any(
            |e| e.0 == "expired_reclaimed" && e.1.as_deref() == Some(run.to_string().as_str())
        ));
        assert_eq!(launcher.calls().len(), 1, "the reopened ticket is launched");
    }

    #[tokio::test]
    async fn a_live_claim_is_left_alone_by_the_sweep() {
        let pool = crate::testdb::fresh_pool().await;
        let run = insert_run(&pool, "running").await;
        let holder = Holder {
            kind: HolderKind::Run,
            id: run.to_string(),
            worktree: "/wt/run".to_owned(),
        };
        let id = claim_as(&pool, &holder, "file:core/a.rs", t0() - mins(120)).await;
        let launcher = FakeLauncher::default();
        tick(&pool, &launcher, t0()).await.unwrap();
        assert_eq!(task(&pool, id).await.status, Status::Claimed);
        assert!(
            events(&pool, id)
                .await
                .iter()
                .all(|e| e.0 != "expired_reclaimed")
        );
        assert!(launcher.calls().is_empty());
    }

    #[tokio::test]
    async fn a_problem_that_comes_back_after_two_launches_is_parked() {
        let pool = crate::testdb::fresh_pool().await;
        let first = open_ticket(&pool, "file:core/a.rs", t0() - mins(60)).await;
        close_launched(&pool, first, 2, t0() - mins(30)).await;
        let back = open_ticket(&pool, "file:core/a.rs", t0() - mins(1)).await;
        assert_ne!(first, back, "a closed key opens a new row");
        // A key whose two launches are older than the window starts over.
        let old = open_ticket(&pool, "file:core/c.rs", t0() - Duration::days(9)).await;
        close_launched(&pool, old, 2, t0() - Duration::days(8)).await;
        let fresh = open_ticket(&pool, "file:core/c.rs", t0() - mins(20)).await;

        let launcher = FakeLauncher::default();
        tick(&pool, &launcher, t0()).await.unwrap();

        assert_eq!(launcher.calls(), [(fresh, LaunchKind::Leaf, 1)]);
        let back_row = task(&pool, back).await;
        assert_eq!(back_row.parked_at, Some(stamp(t0())));
        assert_eq!(back_row.launch_attempts, 0, "no third launch");
        assert_eq!(task(&pool, fresh).await.parked_at, None);
    }

    #[tokio::test]
    async fn parking_writes_one_decision_with_no_tree() {
        let pool = crate::testdb::fresh_pool().await;
        let id = open_ticket(&pool, "file:core/a.rs", t0() - mins(11)).await;
        let launcher = FakeLauncher::default();
        for at in [0, 10, 20, 30, 40] {
            tick(&pool, &launcher, t0() + mins(at)).await.unwrap();
        }
        let decisions: Vec<(String, Option<String>, Option<String>, String)> =
            sqlx::query_as("SELECT id, tree_id, node_id, question FROM decisions")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(decisions.len(), 1);
        let (decision_id, tree_id, node_id, question) = &decisions[0];
        assert_eq!(decision_id, &format!("ticket-{id}"));
        assert_eq!((tree_id, node_id), (&None, &None));
        assert!(question.contains(&format!("#{id}")) && question.contains("file:core/a.rs"));
        let parked = events(&pool, id)
            .await
            .into_iter()
            .filter(|e| e.0 == "parked")
            .count();
        assert_eq!(parked, 1);
    }
}
