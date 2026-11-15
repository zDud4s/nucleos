//! The log of rule firings: what each automation did for each event, once per event.
//!
//! `(project_id, rule, event_key)` is unique, so the postgate re-emitting the same green every tick records one
//! firing and queues one push. The CI watch reads [`last_push`] to know which sha an automation pushed.

use sqlx::SqlitePool;

pub const QUEUED: &str = "queued";
pub const PUSHED: &str = "pushed";
pub const REJECTED: &str = "rejected";
pub const DIVERGED: &str = "diverged";
pub const PAUSED: &str = "paused";
pub const SKIPPED: &str = "skipped";
pub const FAILED: &str = "failed";
pub const UNSUPPORTED: &str = "unsupported";
/// An `open_incident` firing that carried out its action.
pub const OK: &str = "ok";
/// An `open_incident` firing that could not reach `gh`: no incident, the rule stays on.
pub const CI_UNAVAILABLE: &str = "ci_unavailable";

/// The feed kind of a firing's line.
pub const FEED_KIND: &str = "automation_firing";

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Firing {
    pub id: i64,
    pub project_id: String,
    pub rule: String,
    pub event_key: String,
    pub branch: String,
    pub sha: String,
    pub action: String,
    pub outcome: String,
    pub detail: String,
    pub at: String,
}

#[derive(Debug, Clone, Copy)]
pub struct NewFiring<'a> {
    pub project_id: &'a str,
    pub rule: &'a str,
    pub event_key: &'a str,
    pub branch: &'a str,
    pub sha: &'a str,
    pub action: &'a str,
    pub outcome: &'a str,
    pub detail: &'a str,
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Records a firing. `None` means this rule already fired for this event: nothing was written.
pub async fn record(pool: &SqlitePool, new: &NewFiring<'_>) -> sqlx::Result<Option<Firing>> {
    sqlx::query_as::<_, Firing>(
        "INSERT INTO automation_firings
             (project_id, rule, event_key, branch, sha, action, outcome, detail, at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (project_id, rule, event_key) DO NOTHING
         RETURNING id, project_id, rule, event_key, branch, sha, action, outcome, detail, at",
    )
    .bind(new.project_id)
    .bind(new.rule)
    .bind(new.event_key)
    .bind(new.branch)
    .bind(new.sha)
    .bind(new.action)
    .bind(new.outcome)
    .bind(new.detail)
    .bind(now())
    .fetch_optional(pool)
    .await
}

/// Moves the newest still-`queued` push firing of `(project, rule, sha)` to `outcome`. `None` when there is none.
pub async fn settle(
    pool: &SqlitePool,
    project_id: &str,
    rule: &str,
    sha: &str,
    outcome: &str,
    detail: &str,
) -> sqlx::Result<Option<Firing>> {
    sqlx::query_as::<_, Firing>(
        "UPDATE automation_firings SET outcome = ?, detail = ?
         WHERE id = (SELECT id FROM automation_firings
                     WHERE project_id = ? AND rule = ? AND sha = ? AND action = 'push' AND outcome = 'queued'
                     ORDER BY id DESC LIMIT 1)
         RETURNING id, project_id, rule, event_key, branch, sha, action, outcome, detail, at",
    )
    .bind(outcome)
    .bind(detail)
    .bind(project_id)
    .bind(rule)
    .bind(sha)
    .fetch_optional(pool)
    .await
}

/// The newest push firing of `branch` that actually pushed, whichever rule it was.
pub async fn last_push(
    pool: &SqlitePool,
    project_id: &str,
    branch: &str,
) -> sqlx::Result<Option<Firing>> {
    sqlx::query_as::<_, Firing>(
        "SELECT id, project_id, rule, event_key, branch, sha, action, outcome, detail, at FROM automation_firings
         WHERE project_id = ? AND branch = ? AND action = 'push' AND outcome = 'pushed'
         ORDER BY id DESC LIMIT 1",
    )
    .bind(project_id)
    .bind(branch)
    .fetch_optional(pool)
    .await
}

/// The distinct branches that have at least one rule push with outcome `pushed`.
pub async fn push_branches(pool: &SqlitePool, project_id: &str) -> sqlx::Result<Vec<String>> {
    sqlx::query_scalar(
        "SELECT DISTINCT branch FROM automation_firings
         WHERE project_id = ? AND action = 'push' AND outcome = 'pushed'
         ORDER BY branch",
    )
    .bind(project_id)
    .fetch_all(pool)
    .await
}

/// One free-text feed line (kind [`FEED_KIND`]). Best effort: a failed write is only logged.
pub async fn note(pool: &SqlitePool, project_id: &str, text: &str) {
    if let Err(error) =
        crate::feed::append(pool, Some(project_id), FEED_KIND, text, None, None).await
    {
        tracing::warn!(%error, project_id, "could not write an automation feed note");
    }
}

/// One feed line for a firing. Best effort: a failed write is only logged.
pub async fn announce(pool: &SqlitePool, firing: &Firing) {
    let short = firing.sha.get(..7).unwrap_or(&firing.sha);
    let mut text = format!(
        "automation `{}`: {} {} at {} — {}",
        firing.rule, firing.action, firing.branch, short, firing.outcome
    );
    if !firing.detail.is_empty() {
        text.push_str(&format!(" ({})", firing.detail));
    }
    note(pool, &firing.project_id, &text).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push<'a>(
        project: &'a str,
        rule: &'a str,
        key: &'a str,
        branch: &'a str,
        sha: &'a str,
    ) -> NewFiring<'a> {
        NewFiring {
            project_id: project,
            rule,
            event_key: key,
            branch,
            sha,
            action: "push",
            outcome: QUEUED,
            detail: "",
        }
    }

    #[tokio::test]
    async fn the_first_record_returns_the_firing_and_a_repeat_returns_none() {
        let pool = crate::testdb::fresh_pool().await;
        let new = push("p", "r", "local_green:abc", "master", "abc");
        let first = record(&pool, &new).await.unwrap().expect("first firing");
        assert_eq!(first.outcome, QUEUED);
        assert_eq!(first.event_key, "local_green:abc");
        assert!(record(&pool, &new).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn the_same_event_key_for_another_rule_or_project_is_a_new_firing() {
        let pool = crate::testdb::fresh_pool().await;
        record(&pool, &push("p", "r", "k", "master", "abc"))
            .await
            .unwrap()
            .unwrap();
        assert!(
            record(&pool, &push("p", "other", "k", "master", "abc"))
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            record(&pool, &push("q", "r", "k", "master", "abc"))
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn settle_moves_a_queued_push_to_the_outcome_and_keeps_its_detail() {
        let pool = crate::testdb::fresh_pool().await;
        record(&pool, &push("p", "r", "k", "master", "abc"))
            .await
            .unwrap()
            .unwrap();
        let settled = settle(&pool, "p", "r", "abc", PUSHED, "fast-forward")
            .await
            .unwrap()
            .expect("settled");
        assert_eq!(settled.outcome, PUSHED);
        assert_eq!(settled.detail, "fast-forward");
    }

    #[tokio::test]
    async fn settle_does_not_touch_a_firing_that_is_no_longer_queued() {
        let pool = crate::testdb::fresh_pool().await;
        record(&pool, &push("p", "r", "k", "master", "abc"))
            .await
            .unwrap()
            .unwrap();
        settle(&pool, "p", "r", "abc", PUSHED, "")
            .await
            .unwrap()
            .unwrap();
        assert!(
            settle(&pool, "p", "r", "abc", REJECTED, "late")
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn last_push_is_the_newest_pushed_firing_of_the_branch_whatever_the_rule() {
        let pool = crate::testdb::fresh_pool().await;
        record(&pool, &push("p", "a", "k1", "master", "s1"))
            .await
            .unwrap();
        settle(&pool, "p", "a", "s1", PUSHED, "").await.unwrap();
        record(&pool, &push("p", "b", "k2", "master", "s2"))
            .await
            .unwrap();
        settle(&pool, "p", "b", "s2", PUSHED, "").await.unwrap();
        // Ignored: not pushed, another branch, another project, not a push.
        record(&pool, &push("p", "a", "k3", "master", "s3"))
            .await
            .unwrap();
        settle(&pool, "p", "a", "s3", REJECTED, "").await.unwrap();
        record(&pool, &push("p", "a", "k4", "release", "s4"))
            .await
            .unwrap();
        settle(&pool, "p", "a", "s4", PUSHED, "").await.unwrap();
        record(&pool, &push("q", "a", "k5", "master", "s5"))
            .await
            .unwrap();
        settle(&pool, "q", "a", "s5", PUSHED, "").await.unwrap();
        let mut incident = push("p", "c", "k6", "master", "s6");
        incident.action = "open_incident";
        incident.outcome = PUSHED;
        record(&pool, &incident).await.unwrap();

        let last = last_push(&pool, "p", "master")
            .await
            .unwrap()
            .expect("a push");
        assert_eq!(last.sha, "s2");
        assert_eq!(last.rule, "b");
        assert!(last_push(&pool, "p", "nowhere").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn push_branches_lists_each_branch_with_a_pushed_push_once() {
        let pool = crate::testdb::fresh_pool().await;
        record(&pool, &push("p", "a", "k1", "master", "s1"))
            .await
            .unwrap();
        settle(&pool, "p", "a", "s1", PUSHED, "").await.unwrap();
        record(&pool, &push("p", "a", "k2", "master", "s2"))
            .await
            .unwrap();
        settle(&pool, "p", "a", "s2", PUSHED, "").await.unwrap();
        record(&pool, &push("p", "a", "k3", "release", "s3"))
            .await
            .unwrap();
        settle(&pool, "p", "a", "s3", PUSHED, "").await.unwrap();
        // Never pushed: only queued, only rejected.
        record(&pool, &push("p", "a", "k4", "queued-only", "s4"))
            .await
            .unwrap();
        record(&pool, &push("p", "a", "k5", "rejected-only", "s5"))
            .await
            .unwrap();
        settle(&pool, "p", "a", "s5", REJECTED, "").await.unwrap();
        // Another project.
        record(&pool, &push("q", "a", "k6", "other", "s6"))
            .await
            .unwrap();
        settle(&pool, "q", "a", "s6", PUSHED, "").await.unwrap();

        assert_eq!(
            push_branches(&pool, "p").await.unwrap(),
            vec!["master".to_string(), "release".to_string()]
        );
        assert!(push_branches(&pool, "nobody").await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn last_push_never_returns_a_rejected_or_queued_firing() {
        let pool = crate::testdb::fresh_pool().await;
        record(&pool, &push("p", "a", "k1", "master", "s1"))
            .await
            .unwrap();
        record(&pool, &push("p", "a", "k2", "master", "s2"))
            .await
            .unwrap();
        settle(&pool, "p", "a", "s2", REJECTED, "").await.unwrap();
        assert!(last_push(&pool, "p", "master").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn note_appends_one_feed_line_of_kind_automation_firing() {
        let pool = crate::testdb::fresh_pool().await;
        note(&pool, "p", "ci watch: nothing to report").await;
        let rows: Vec<(String, String)> =
            sqlx::query_as("SELECT kind, summary FROM feed WHERE project_id = ?")
                .bind("p")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(
            rows,
            vec![(
                FEED_KIND.to_string(),
                "ci watch: nothing to report".to_string()
            )]
        );
    }
}
