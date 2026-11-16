//! A job node that dies on an account's usage limit: re-pick another account now, or park until the
//! earliest reset. Never a blind retry against the same exhausted account.

use super::tests::{seed_job, seed_node, seed_worktree, test_pool, test_state};
use super::*;
use crate::accounts::pool::PoolState;
use crate::accounts::provider::LimitHit;
use crate::accounts::{Account, AccountId, AccountKind, AccountState};

/// The process-wide pool is shared by every test in the binary: one test at a time owns it, and it
/// goes back to empty (today's launch) when the test ends.
static GLOBAL_POOL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct PoolOwner<'a>(#[allow(dead_code)] tokio::sync::MutexGuard<'a, ()>);

impl Drop for PoolOwner<'_> {
    fn drop(&mut self) {
        PoolState::global().set_accounts(vec![]);
    }
}

async fn own_pool_with(ids: &[&str]) -> PoolOwner<'static> {
    let guard = GLOBAL_POOL.lock().await;
    PoolState::global().set_accounts(
        ids.iter()
            .map(|id| Account {
                id: AccountId((*id).to_owned()),
                engine: "claude".into(),
                provider: "anthropic".into(),
                label: (*id).to_owned(),
                kind: AccountKind::Subscription,
                credential_locator: None,
                state: AccountState::Active,
                created_at: "2026-11-15T00:00:00Z".into(),
                updated_at: "2026-11-15T00:00:00Z".into(),
            })
            .collect(),
    );
    PoolOwner(guard)
}

const USAGE_LIMIT: &str =
    r#"{"type":"result","is_error":true,"result":"You've hit your 5-hour limit · resets 5:50am"}"#;

/// One running item whose node failed on the account limit while running on `account`.
async fn item_that_hit_the_limit(pool: &sqlx::SqlitePool, account: &str) -> i64 {
    let worktree = tempfile::tempdir().unwrap();
    let job_id = seed_job(pool, "project-a", "running").await.unwrap();
    seed_worktree(pool, job_id, worktree.path()).await;
    let run_id = seed_node(pool, job_id, "implement", "failed").await;
    sqlx::query("UPDATE runs SET stdout = ?, runner = 'claude', account_id = ? WHERE id = ?")
        .bind(USAGE_LIMIT)
        .bind(account)
        .bind(run_id)
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO job_nodes (job_id, node_id, ordinal, description, status, run_id)
         VALUES (?, 'implement', 0, 'an item', 'running', ?)",
    )
    .bind(job_id)
    .bind(run_id)
    .execute(pool)
    .await
    .unwrap();
    // Keep the temp dir alive for the test's lifetime: the walk only needs the row.
    std::mem::forget(worktree);
    job_id
}

async fn item_row(pool: &sqlx::SqlitePool, job_id: i64) -> (String, i64, Option<String>) {
    sqlx::query_as(
        "SELECT status, transient_retries, transient_retry_at FROM job_nodes
         WHERE job_id = ? AND ordinal = 0",
    )
    .bind(job_id)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// With another account available the item goes back to `pending` at once: no backoff, and the
/// retry does not count against the transient-error cap.
#[tokio::test]
async fn a_limit_with_another_account_available_runs_the_item_again_now() {
    let _pool_owner = own_pool_with(&["claude:a", "claude:b"]).await;
    let pool = test_pool().await;
    let state = test_state(pool.clone()).await;
    let job_id = item_that_hit_the_limit(&pool, "claude:a").await;

    let job = load_job(&pool, job_id).await.unwrap();
    reconcile_nodes_at(&state, &job, Utc::now()).await.unwrap();

    assert_eq!(
        item_row(&pool, job_id).await,
        ("pending".to_owned(), 0, None)
    );
    // The account that hit its limit is out of the pool; the other one is not.
    let view = PoolState::global().view();
    let exhausted: Vec<_> = view
        .candidates
        .iter()
        .filter(|c| !c.exhausted.is_empty())
        .map(|c| c.account.id.0.clone())
        .collect();
    assert_eq!(exhausted, vec!["claude:a".to_owned()]);
    // And with one account left the job is not braked.
    assert!(account_limit_brake(&job, Utc::now()).is_none());
}

/// With every account at its limit the item is not retried blind: the brake parks the job, with the
/// resume time in the reason a reader sees.
#[tokio::test]
async fn a_limit_on_the_only_account_parks_the_job_until_the_reset() {
    let _pool_owner = own_pool_with(&["claude:a"]).await;
    let pool = test_pool().await;
    let state = test_state(pool.clone()).await;
    let job_id = item_that_hit_the_limit(&pool, "claude:a").await;

    let job = load_job(&pool, job_id).await.unwrap();
    reconcile_nodes_at(&state, &job, Utc::now()).await.unwrap();

    let (status, retries, retry_at) = item_row(&pool, job_id).await;
    assert_eq!(
        (status.as_str(), retries, retry_at),
        ("pending", 0, None),
        "no counted, backed-off retry against the exhausted account"
    );
    // The reset in the stream is "5:50am", the next one on the wall clock: depending on the hour the
    // tests run it can fall beyond the job's lifetime (a stop). Born in the future, the job's
    // lifetime always covers it, so the verdict does not depend on the time of day.
    let mut job = job;
    job.created_at = (Utc::now() + chrono::Duration::days(2)).to_rfc3339();
    match brakes(&state, &job, Utc::now()).await {
        Brake::Park { reason, detail } => {
            assert_eq!(reason, WAIT_ACCOUNT_LIMIT);
            assert!(detail.contains("resumes at"), "{detail}");
        }
        Brake::Stop { detail } => panic!("expected the job to be parked, got a stop: {detail}"),
        _ => panic!("expected the job to be parked"),
    }
}

/// The job resumes by itself once the earliest reset plus the margin has passed, and not a moment
/// before.
#[tokio::test]
async fn the_parked_job_resumes_after_the_reset_plus_the_margin() {
    let _pool_owner = own_pool_with(&["claude:a", "claude:b"]).await;
    let pool = test_pool().await;
    let job_id = seed_job(&pool, "project-a", "running").await.unwrap();
    let job = load_job(&pool, job_id).await.unwrap();
    let now = Utc::now();
    let early = now + chrono::Duration::hours(1);
    let late = now + chrono::Duration::hours(3);
    for (id, reset) in [("claude:a", late), ("claude:b", early)] {
        PoolState::global().mark_exhausted(
            &AccountId(id.to_owned()),
            &LimitHit {
                window: Some("5h".into()),
                resets_at: Some(reset),
            },
            now,
        );
    }

    // Parked until the EARLIEST reset (b), plus the margin.
    assert!(matches!(
        account_limit_brake(&job, now),
        Some(Brake::Park {
            reason: WAIT_ACCOUNT_LIMIT,
            ..
        })
    ));
    assert!(
        matches!(
            account_limit_brake(&job, early + RESUME_MARGIN - chrono::Duration::seconds(5)),
            Some(Brake::Park { .. })
        ),
        "inside the margin the job still waits"
    );
    assert!(
        account_limit_brake(&job, early + RESUME_MARGIN + chrono::Duration::seconds(5)).is_none(),
        "past the earliest reset and the margin the job goes"
    );
}

/// A review (a sequential node) that died on the account limit with a second account free is owed a
/// retry, and the account it died on is out of the pool; with the only account it parks instead.
async fn job_whose_review_hit_the_limit(pool: &sqlx::SqlitePool, account: &str) -> JobRow {
    let job_id = seed_job(pool, "project-a", "running").await.unwrap();
    let run_id = seed_node(pool, job_id, "review", "failed").await;
    sqlx::query("UPDATE runs SET stdout = ?, runner = 'claude', account_id = ? WHERE id = ?")
        .bind(USAGE_LIMIT)
        .bind(account)
        .bind(run_id)
        .execute(pool)
        .await
        .unwrap();
    load_job(pool, job_id).await.unwrap()
}

#[tokio::test]
async fn a_review_that_hit_the_limit_is_owed_a_retry_on_another_account() {
    let _pool_owner = own_pool_with(&["claude:a", "claude:b"]).await;
    let pool = test_pool().await;
    let job = job_whose_review_hit_the_limit(&pool, "claude:a").await;

    assert!(failed_on_a_retryable_error(USAGE_LIMIT));
    assert_eq!(
        review_retry_parked_on_limit(&pool, &job, Utc::now()).await,
        None
    );
    let view = PoolState::global().view();
    assert!(
        view.candidates
            .iter()
            .any(|c| c.account.id.0 == "claude:a" && !c.exhausted.is_empty())
    );
}

#[tokio::test]
async fn a_review_that_hit_the_limit_on_the_only_account_parks_with_the_resume_time() {
    let _pool_owner = own_pool_with(&["claude:a"]).await;
    let pool = test_pool().await;
    let job = job_whose_review_hit_the_limit(&pool, "claude:a").await;

    let detail = review_retry_parked_on_limit(&pool, &job, Utc::now())
        .await
        .expect("every account is out");
    assert!(detail.contains("every account is out"), "{detail}");
}

/// An empty pool (no accounts set up) is today's launch: nothing to park on.
#[tokio::test]
async fn an_empty_pool_never_parks_a_job() {
    let _pool_owner = own_pool_with(&[]).await;
    let pool = test_pool().await;
    let job_id = seed_job(&pool, "project-a", "running").await.unwrap();
    let job = load_job(&pool, job_id).await.unwrap();

    assert!(account_limit_brake(&job, Utc::now()).is_none());
}
