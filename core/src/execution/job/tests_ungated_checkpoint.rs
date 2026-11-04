//! Items passed without a gate (`gate_after_each_item` off) keep their work.
//!
//! Job 1 (2026-11-04): items 0-3 passed unmeasured with no checkpoint, the last item's gate went
//! red past its retries, and `settle_revert` reset the branch to `jobs.head_sha` — the nearest
//! footing — destroying every earlier item's finished edits. These pin the fix: an ungated item is
//! checkpointed, so a revert takes off the failing item's own changes and nothing else, and an item
//! that changed nothing is not called `passed`.

use super::tests::{seed_job, seed_worktree, test_pool, test_state};
use super::*;

fn git(repo: &std::path::Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

/// A real checkout with one seed commit, a job standing on it with per-item gating off and no
/// retry budget, and `count` implemented items. `files` is the plan's guess for every item.
async fn ungated_job(
    pool: &sqlx::SqlitePool,
    checkout: &std::path::Path,
    count: usize,
    files: Option<&str>,
) -> i64 {
    git(checkout, &["init", "-q"]);
    git(checkout, &["config", "user.email", "test@x"]);
    git(checkout, &["config", "user.name", "test"]);
    std::fs::write(checkout.join("seed.txt"), "seed\n").unwrap();
    git(checkout, &["add", "-A"]);
    git(checkout, &["commit", "-q", "-m", "seed"]);
    let head = git(checkout, &["rev-parse", "HEAD"]);

    let job_id = seed_job(pool, "project-a", "running").await.unwrap();
    sqlx::query("UPDATE jobs SET gate_each = 0, gate_retries = 0, head_sha = ? WHERE id = ?")
        .bind(&head)
        .bind(job_id)
        .execute(pool)
        .await
        .unwrap();
    seed_worktree(pool, job_id, checkout).await;
    for ordinal in 0..count {
        sqlx::query(
            "INSERT INTO job_nodes (job_id, node_id, ordinal, description, status, files)
             VALUES (?, 'implement', ?, 'change the code', 'implemented', ?)",
        )
        .bind(job_id)
        .bind(ordinal as i64)
        .bind(files)
        .execute(pool)
        .await
        .unwrap();
    }
    job_id
}

async fn item_row(pool: &sqlx::SqlitePool, job_id: i64, ordinal: i64) -> (String, Option<String>) {
    sqlx::query_as("SELECT status, checkpoint_sha FROM job_nodes WHERE job_id = ? AND ordinal = ?")
        .bind(job_id)
        .bind(ordinal)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The defect itself. Items 0..2 pass unmeasured with edits; item 3's gate is red with no retry
/// left. The revert takes item 3's work off the branch and leaves items 0..2's on it.
#[tokio::test]
async fn a_red_last_item_reverts_only_its_own_work_after_ungated_items() {
    let pool = test_pool().await;
    let state = test_state(pool.clone()).await;
    let checkout = tempfile::tempdir().unwrap();
    let job_id = ungated_job(&pool, checkout.path(), 4, Some(r#"["src/lib.rs"]"#)).await;
    let job = load_job(&pool, job_id).await.unwrap();

    for ordinal in 0..3 {
        std::fs::write(
            checkout.path().join(format!("item{ordinal}.txt")),
            "done\n",
        )
        .unwrap();
        assert_eq!(gate_item(&state, &job, ordinal, 4).await, Pass::Continued);
        let (status, checkpoint) = item_row(&pool, job_id, ordinal as i64).await;
        assert_eq!(status, "passed", "item {ordinal}");
        assert!(
            checkpoint.is_some(),
            "an ungated item {ordinal} is checkpointed so a later revert cannot take it"
        );
    }
    // The checkpoint says in git that no gate agreed with it.
    let subject = git(checkout.path(), &["log", "-1", "--format=%s"]);
    assert!(subject.contains("no gate"), "{subject}");

    std::fs::write(checkout.path().join("item3.txt"), "broken\n").unwrap();
    let red = crate::gate::GateOutcome::Failed {
        exit_code: 1,
        output: "boom".into(),
    };
    assert_eq!(record_gate(&state, &job, 3, red).await, Pass::Continued);

    assert_eq!(item_row(&pool, job_id, 3).await.0, "gate_failed");
    for ordinal in 0..3 {
        assert!(
            checkout.path().join(format!("item{ordinal}.txt")).exists(),
            "item {ordinal}'s finished work survived the revert"
        );
    }
    assert!(
        !checkout.path().join("item3.txt").exists(),
        "the failing item's own work was taken off"
    );
    let (_, item2_checkpoint) = item_row(&pool, job_id, 2).await;
    assert_eq!(
        Some(git(checkout.path(), &["rev-parse", "HEAD"])),
        item2_checkpoint,
        "the branch stands on the previous item's checkpoint"
    );
}

/// A code item (the plan named files for it) whose run left the tree exactly where the previous
/// item left it did not do its work, whatever its run said: it is `failed`, not `passed`.
#[tokio::test]
async fn an_ungated_code_item_that_changed_nothing_is_not_passed() {
    let pool = test_pool().await;
    let state = test_state(pool.clone()).await;
    let checkout = tempfile::tempdir().unwrap();
    let job_id = ungated_job(&pool, checkout.path(), 2, Some(r#"["src/lib.rs"]"#)).await;
    let job = load_job(&pool, job_id).await.unwrap();

    assert_eq!(gate_item(&state, &job, 0, 2).await, Pass::Continued);

    let (status, checkpoint) = item_row(&pool, job_id, 0).await;
    assert_eq!(status, "failed");
    assert_eq!(checkpoint, None, "nothing was committed for it");
    let said: Vec<String> =
        sqlx::query_scalar("SELECT summary FROM feed WHERE kind = 'job_item_failed'")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(said.len(), 1, "{said:?}");
    assert!(said[0].contains("changed nothing"), "{}", said[0]);
}

/// An item the plan named no files for may honestly be a no-op (a check, a confirmation), so an
/// unchanged tree alone does not fail it.
#[tokio::test]
async fn an_ungated_item_with_no_files_that_changed_nothing_still_passes() {
    let pool = test_pool().await;
    let state = test_state(pool.clone()).await;
    let checkout = tempfile::tempdir().unwrap();
    let job_id = ungated_job(&pool, checkout.path(), 2, None).await;
    let job = load_job(&pool, job_id).await.unwrap();

    assert_eq!(gate_item(&state, &job, 0, 2).await, Pass::Continued);

    assert_eq!(item_row(&pool, job_id, 0).await.0, "passed");
}
