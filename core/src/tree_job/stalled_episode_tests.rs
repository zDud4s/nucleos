//! One `work_tree_stalled` feed line per stall episode, and the decision run before the owner is asked.

use chrono::{DateTime, Utc};

use super::{ANNOUNCED_KEY, STALL_SECS, announced, check, mark_announced, store};
use crate::tree_job::driver::tests::{Fixture, code_branch};

const JOB: &str = "episode-job";

async fn feed_lines(fx: &Fixture) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM feed WHERE kind = 'work_tree_stalled'")
        .fetch_one(&fx.state.pool)
        .await
        .unwrap()
}

fn step(at: DateTime<Utc>) -> DateTime<Utc> {
    at + chrono::Duration::minutes(STALL_SECS / 60 + 1)
}

#[test]
fn an_episode_is_announced_only_while_the_job_is_as_the_detector_left_it() {
    let mut job = crate::tree_job::store::TreeJob {
        id: "j".to_owned(),
        root_node_id: "r".to_owned(),
        project_id: None,
        feature_branch: None,
        goal: "g".to_owned(),
        state: "open".to_owned(),
        manifest: crate::tree_job::model::Manifest::new("j", "feat/x", "g", "/repo", &[]),
        contract: None,
        created_at: "2026-01-01T00:00:00Z".to_owned(),
        updated_at: "2026-01-01T00:00:00Z".to_owned(),
    };
    assert!(!announced(&job));
    mark_announced(&mut job);
    assert!(announced(&job));
    job.updated_at = "2026-01-01T01:00:00Z".to_owned();
    assert!(!announced(&job), "progress ends the episode");
    assert!(job.manifest.extra.contains_key(ANNOUNCED_KEY));
}

#[tokio::test]
async fn every_rung_of_one_episode_shares_one_feed_line() {
    let fx = Fixture::new().await;
    fx.seed_job(JOB, vec![code_branch("api", "green", None)], None)
        .await;

    let mut at = Utc::now();
    for _ in 0..4 {
        at = step(at);
        check(&fx.state, JOB, at).await.unwrap();
    }

    assert_eq!(feed_lines(&fx).await, 1);
}

#[tokio::test]
async fn progress_between_two_stalls_gives_each_its_own_line() {
    let fx = Fixture::new().await;
    fx.seed_job(JOB, vec![code_branch("api", "green", None)], None)
        .await;

    let mut at = step(Utc::now());
    check(&fx.state, JOB, at).await.unwrap();
    at = step(at);
    check(&fx.state, JOB, at).await.unwrap();
    assert_eq!(feed_lines(&fx).await, 1);

    // The tree moves: somebody else rewrites the job.
    let mut job = fx.job(JOB).await;
    job.updated_at = at.to_rfc3339_opts(chrono::SecondsFormat::Secs, false);
    store::update(&fx.state.pool, &job).await.unwrap();

    at = step(at);
    check(&fx.state, JOB, at).await.unwrap();
    assert_eq!(feed_lines(&fx).await, 2);
}

#[tokio::test]
async fn with_the_flag_off_the_ladder_is_the_same_and_still_one_line() {
    let fx = Fixture::new().await;
    fx.seed_job(JOB, vec![code_branch("api", "green", None)], None)
        .await;
    let dir = std::env::temp_dir().join(format!("stalled-episode-{}", std::process::id()));
    std::fs::create_dir_all(dir.join(".nucleos/workflow")).unwrap();
    std::fs::write(
        dir.join(".nucleos/workflow/speed.yaml"),
        "autonomy:\n  tree_decides_itself: false\n",
    )
    .unwrap();
    let mut job = fx.job(JOB).await;
    job.manifest.project_root = dir.to_string_lossy().into_owned();
    store::update(&fx.state.pool, &job).await.unwrap();

    let mut at = Utc::now();
    for _ in 0..6 {
        at = step(at);
        check(&fx.state, JOB, at).await.unwrap();
    }

    assert_eq!(feed_lines(&fx).await, 1);
    std::fs::remove_dir_all(dir).ok();
}
