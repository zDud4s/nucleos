//! The `local_green` event of the project automations (spec 2026-11-15, plan A T6).
//!
//! Without a release branch, the green sha of the integration branch is what a `push` rule on
//! `published_green` answers; with one, the `Promote` executor emits `promoted` instead. No state of
//! its own: a firing is recorded once per rule and sha (`automations::firings`), so emitting the same
//! green on every tick queues one push.

use std::path::Path;

use crate::automations::{self, Event};
use crate::config::AutopilotRules;
use crate::execution::git_exec;
use crate::verify::postgate::{covers_local_targets, State};

/// Emits `local_green` for the gate's green sha when there is one and no release branch.
pub(super) async fn emit_local_green_if_due(
    pool: &sqlx::SqlitePool,
    project_id: &str,
    project_root: &Path,
    state: &State,
    rules: &AutopilotRules,
) {
    if rules.release_branch().is_some() || !covers_local_targets(state, rules) {
        return;
    }
    let deadline = std::time::Instant::now() + git_exec::OPERATION_TIMEOUT;
    let Some(sha) = state.promotable_green(project_root, deadline).await else {
        return;
    };
    automations::dispatch(
        pool,
        project_id,
        Event::LocalGreen {
            sha,
            branch: state.target.clone(),
        },
    )
    .await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project::rules::testing::publish_automations;
    use crate::verify::postgate as verify_postgate;
    use super::super::tests::{fixture, Fixture, GATE_ON, RELEASE_ON};

    const RULE_ON: &str = "rules:\n  - name: push-when-green\n    enabled: true\n    on: {event: published_green}\n    do: {action: push, remote: origin}\n";
    const RULE_OFF: &str = "rules:\n  - name: push-when-green\n    enabled: false\n    on: {event: published_green}\n    do: {action: push, remote: origin}\n";

    /// Marks `main`'s tip green and returns it.
    async fn main_is_green(f: &Fixture) -> String {
        verify_postgate::mark_covered(&f.pool, "alpha", "main", &f.tip)
            .await
            .unwrap();
        f.tip.clone()
    }

    async fn emit(f: &Fixture) {
        let state = verify_postgate::load(&f.pool, "alpha")
            .await
            .unwrap()
            .expect("a gate state");
        let rules = crate::project::rules::autopilot("alpha").expect("the stored rules");
        emit_local_green_if_due(&f.pool, "alpha", &f.root, &state, &rules).await;
    }

    /// `(sha, branch)` of every queued `push-sha`.
    async fn pushes(f: &Fixture) -> Vec<(String, String)> {
        sqlx::query_as(
            "SELECT json_extract(args, '$.sha'), json_extract(args, '$.branch') FROM vcs_requests
             WHERE op = 'push-sha' ORDER BY id",
        )
        .fetch_all(&f.pool)
        .await
        .unwrap()
    }

    async fn firings(f: &Fixture) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM automation_firings")
            .fetch_one(&f.pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn a_green_sha_with_the_rule_on_queues_one_push_sha() {
        let f = fixture(Some(GATE_ON)).await;
        publish_automations("alpha", RULE_ON);
        let green = main_is_green(&f).await;

        emit(&f).await;

        assert_eq!(pushes(&f).await, vec![(green, "main".to_owned())]);
    }

    #[tokio::test]
    async fn emitting_the_same_green_on_every_tick_queues_one_push_only() {
        let f = fixture(Some(GATE_ON)).await;
        publish_automations("alpha", RULE_ON);
        main_is_green(&f).await;

        for _ in 0..3 {
            emit(&f).await;
        }

        assert_eq!(firings(&f).await, 1);
        assert_eq!(pushes(&f).await.len(), 1);
    }

    #[tokio::test]
    async fn with_a_release_branch_set_nothing_is_emitted_here() {
        let f = fixture(Some(RELEASE_ON)).await;
        publish_automations("alpha", RULE_ON);
        main_is_green(&f).await;

        emit(&f).await;

        assert_eq!(firings(&f).await, 0, "the promote is the emitter");
        assert!(pushes(&f).await.is_empty());
    }

    #[tokio::test]
    async fn with_the_rule_off_nothing_is_queued_even_on_green() {
        let f = fixture(Some(GATE_ON)).await;
        publish_automations("alpha", RULE_OFF);
        main_is_green(&f).await;

        emit(&f).await;

        assert_eq!(firings(&f).await, 0);
        assert!(pushes(&f).await.is_empty());
    }

    #[tokio::test]
    async fn a_red_or_not_yet_green_state_emits_nothing() {
        let f = fixture(Some(GATE_ON)).await;
        publish_automations("alpha", RULE_ON);
        main_is_green(&f).await;
        sqlx::query(
            "UPDATE postgate_state SET last_green_sha = NULL, covered_sha = NULL WHERE project_id = 'alpha'",
        )
        .execute(&f.pool)
        .await
        .unwrap();

        emit(&f).await;

        assert_eq!(firings(&f).await, 0);
        assert!(pushes(&f).await.is_empty());
    }
}
