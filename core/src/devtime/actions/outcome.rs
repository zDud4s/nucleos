//! Closes the `inject` rows of `devtime_shadow` (actions design spec, section 3.4). Shadow is the
//! control group: nothing was injected, so what the session did is what it would have done anyway.
//!
//! Contract of [`close_inject_outcomes`] (the GREEN step implements exactly this):
//! - Considers rows with `action = 'inject'`, `outcome_state = 'pending'` and
//!   `window_end_at <= ms_text(now_ms)` (via `store::pending_due`), in both modes and for `fire` and
//!   `suppress` decisions alike. `propose` rows and rows not yet due are left untouched.
//! - Looks at the findings of the row's `session_id` and `rule_id` (`store::session_findings`):
//!   - some have `rule_version == row.rule_version`: `resolved` / `hit`, `outcome_finding_key` = the
//!     key of the costliest such finding (ties: smallest key), `outcome_cost_ms` = the sum of their
//!     `cost_ms`;
//!   - none of that version, but the rule has findings of another version: the supporting finding is
//!     gone, so the row goes `void` (`store::void_pending`), with no outcome;
//!   - no finding of the rule at all: `resolved` / `miss`, no key, `outcome_cost_ms` = 0.
//! - Resolved rows also get `outcome_verified` = [`aggregate_verified`] over the session's
//!   `devtime_attempt_marks` (informative only). `void` rows get none.
//! - A row without `session_id` cannot be matched: it is `void`.
//! - `human_verdict` is never touched. Closing is idempotent: a second call changes nothing.

use sqlx::SqlitePool;

use crate::devtime::store;

/// How many rows one call closed, by result.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CloseCounts {
    pub hit: usize,
    pub miss: usize,
    pub void: usize,
}

/// The session's `verified` from its attempt marks: `failed` if any mark failed, else `passed` if any
/// passed, else `not_measured` (also for no marks at all).
pub fn aggregate_verified(marks: &[String]) -> &'static str {
    if marks.iter().any(|m| m == "failed") {
        "failed"
    } else if marks.iter().any(|m| m == "passed") {
        "passed"
    } else {
        "not_measured"
    }
}

/// Closes every due pending `inject` row at `now_ms` (unix milliseconds). See the module contract.
pub async fn close_inject_outcomes(pool: &SqlitePool, now_ms: i64) -> sqlx::Result<CloseCounts> {
    let now = super::ms_text(now_ms);
    let mut counts = CloseCounts::default();
    for row in store::pending_due(pool, &now).await? {
        if row.action != "inject" {
            continue;
        }
        let Some(session_id) = row.session_id.as_deref() else {
            counts.void += changed_count(store::void_pending(pool, row.id).await?);
            continue;
        };
        let findings = store::session_findings(pool, session_id).await?;
        let of_rule: Vec<&store::SessionFinding> = findings
            .iter()
            .filter(|f| f.rule_id == row.rule_id)
            .collect();
        let same_version: Vec<&store::SessionFinding> = of_rule
            .iter()
            .copied()
            .filter(|f| f.rule_version == row.rule_version)
            .collect();
        if same_version.is_empty() && !of_rule.is_empty() {
            counts.void += changed_count(store::void_pending(pool, row.id).await?);
            continue;
        }
        // Findings arrive ordered by key, so the first of equal costs is the smallest key.
        let mut costliest: Option<&store::SessionFinding> = None;
        for f in same_version.iter().copied() {
            if costliest.is_none_or(|best| f.cost_ms > best.cost_ms) {
                costliest = Some(f);
            }
        }
        let marks = store::session_attempt_verified(pool, session_id).await?;
        let hit = costliest.is_some();
        let outcome = store::ShadowOutcome {
            state: "resolved".into(),
            outcome: Some(if hit { "hit" } else { "miss" }.into()),
            finding_key: costliest.map(|f| f.finding_key.clone()),
            cost_ms: Some(same_version.iter().map(|f| f.cost_ms).sum()),
            verified: Some(aggregate_verified(&marks).into()),
            proposal_id: None,
            resolved_at: now.clone(),
        };
        if store::resolve(pool, row.id, &outcome).await? {
            if hit {
                counts.hit += 1;
            } else {
                counts.miss += 1;
            }
        }
    }
    Ok(counts)
}

fn changed_count(changed: bool) -> usize {
    usize::from(changed)
}

/// How many `propose` rows one call closed, by result.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ProposeCloseCounts {
    pub hit: usize,
    pub miss: usize,
    pub accepted: usize,
    pub rejected: usize,
    pub ignored: usize,
    pub expired: usize,
}

/// Closes every due pending `propose` row at `now_ms` (unix milliseconds). A human verdict prevails;
/// else shadow rows look for the finding reappearing inside the window and live rows read the
/// proposal's status. A live row with no usable proposal (no id, or the proposal is gone) has no
/// signal and `expired`s.
pub async fn close_propose_outcomes(
    pool: &SqlitePool,
    now_ms: i64,
) -> sqlx::Result<ProposeCloseCounts> {
    let now = super::ms_text(now_ms);
    let mut counts = ProposeCloseCounts::default();
    for row in store::pending_due(pool, &now).await? {
        if row.action != "propose" {
            continue;
        }
        let mut finding_key = None;
        let outcome = match row.human_verdict.as_deref() {
            Some("useful") => Some("accepted"),
            Some("noise") => Some("rejected"),
            _ if row.mode == "live" => {
                let status = match row.proposal_id {
                    Some(id) => store::proposal_status(pool, id).await?,
                    None => None,
                };
                match status.as_deref() {
                    Some("approved") => Some("accepted"),
                    Some("rejected") => Some("rejected"),
                    Some(_) => Some("ignored"),
                    None => None,
                }
            }
            _ => {
                finding_key = store::cross_reappearance(
                    pool,
                    &row.subject_key,
                    &row.decided_at,
                    &row.window_end_at,
                )
                .await?;
                Some(if finding_key.is_some() { "hit" } else { "miss" })
            }
        };
        let closing = store::ShadowOutcome {
            state: if outcome.is_some() {
                "resolved"
            } else {
                "expired"
            }
            .into(),
            outcome: outcome.map(Into::into),
            finding_key,
            proposal_id: row.proposal_id,
            resolved_at: now.clone(),
            ..store::ShadowOutcome::default()
        };
        if !store::resolve(pool, row.id, &closing).await? {
            continue;
        }
        match outcome {
            Some("hit") => counts.hit += 1,
            Some("miss") => counts.miss += 1,
            Some("accepted") => counts.accepted += 1,
            Some("rejected") => counts.rejected += 1,
            Some(_) => counts.ignored += 1,
            None => counts.expired += 1,
        }
    }
    Ok(counts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devtime::store as st;

    const SID: &str = "s-out";
    /// `2026-10-05T10:30:00.000Z`, the end of the session window used below.
    const WINDOW_END_MS: i64 = 1_791_196_200_000;

    async fn pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    async fn pending_row(pool: &SqlitePool, rule: &str, rule_version: i64) -> i64 {
        let row = st::ShadowRow {
            action: "inject".into(),
            rule_id: rule.into(),
            project_id: "p1".into(),
            session_id: Some(SID.into()),
            subject_key: format!("{rule}|p1"),
            mode: "shadow".into(),
            decision: "fire".into(),
            template_id: "inject.d3.v1".into(),
            rule_version,
            action_version: 1,
            thresholds_hash: "th".into(),
            rules_version: "rv".into(),
            decided_at: "2026-10-05T10:00:00.000Z".into(),
            window_end_at: "2026-10-05T10:30:00.000Z".into(),
            outcome_state: "pending".into(),
            ..st::ShadowRow::default()
        };
        assert!(st::insert_decision(pool, &row).await.unwrap());
        sqlx::query_scalar("SELECT id FROM devtime_shadow WHERE rule_id = ? AND rule_version = ?")
            .bind(rule)
            .bind(rule_version)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn finding(pool: &SqlitePool, key: &str, rule: &str, version: i64, cost_ms: i64) {
        sqlx::query(
            "INSERT INTO devtime_findings (finding_key, project_id, session_id, scope, rule_id, \
             rule_version, level, waste, lever, confidence, lane, started_at, ended_at, cost_ms, \
             count, sessions, rules_version, parser_version) VALUES (?, 'p1', ?, 'session', ?, ?, \
             'base', 'rework', 'x', 'exact', 'main', '2026-10-05T10:10:00.000Z', \
             '2026-10-05T10:11:00.000Z', ?, 1, '[]', 'rv', 1)",
        )
        .bind(key)
        .bind(SID)
        .bind(rule)
        .bind(version)
        .bind(cost_ms)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn mark(pool: &SqlitePool, attempt: &str, verified: &str) {
        sqlx::query(
            "INSERT INTO devtime_attempt_marks (attempt_id, session_id, verified, rules_version) \
             VALUES (?, ?, ?, 'rv')",
        )
        .bind(attempt)
        .bind(SID)
        .bind(verified)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn row(pool: &SqlitePool, id: i64) -> st::ShadowRow {
        sqlx::query_as::<_, st::ShadowRow>("SELECT * FROM devtime_shadow WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn outcome_hit_records_finding_key_and_cost() {
        let pool = pool().await;
        let id = pending_row(&pool, "D3", 1).await;
        finding(&pool, "D3-a", "D3", 1, 40_000).await;
        finding(&pool, "D3-b", "D3", 1, 70_000).await;
        finding(&pool, "D6-x", "D6", 1, 999_000).await;

        let counts = close_inject_outcomes(&pool, WINDOW_END_MS + 1)
            .await
            .unwrap();
        assert_eq!(
            counts,
            CloseCounts {
                hit: 1,
                miss: 0,
                void: 0
            }
        );
        let got = row(&pool, id).await;
        assert_eq!(got.outcome_state, "resolved");
        assert_eq!(got.outcome.as_deref(), Some("hit"));
        assert_eq!(got.outcome_finding_key.as_deref(), Some("D3-b"));
        assert_eq!(got.outcome_cost_ms, Some(110_000));
        assert!(got.resolved_at.is_some());
    }

    #[tokio::test]
    async fn outcome_miss_when_the_session_has_no_finding_of_the_rule() {
        let pool = pool().await;
        let id = pending_row(&pool, "D3", 1).await;
        finding(&pool, "D6-x", "D6", 1, 50_000).await;

        let counts = close_inject_outcomes(&pool, WINDOW_END_MS).await.unwrap();
        assert_eq!(counts.miss, 1);
        let got = row(&pool, id).await;
        assert_eq!(got.outcome_state, "resolved");
        assert_eq!(got.outcome.as_deref(), Some("miss"));
        assert_eq!(got.outcome_finding_key, None);
        assert_eq!(got.outcome_cost_ms, Some(0));
    }

    #[tokio::test]
    async fn outcome_verified_aggregates_the_session_attempt_marks() {
        let pool = pool().await;
        let id = pending_row(&pool, "D3", 1).await;
        finding(&pool, "D3-a", "D3", 1, 40_000).await;
        mark(&pool, "a1", "passed").await;
        mark(&pool, "a2", "failed").await;
        mark(&pool, "a3", "not_measured").await;

        close_inject_outcomes(&pool, WINDOW_END_MS).await.unwrap();
        assert_eq!(
            row(&pool, id).await.outcome_verified.as_deref(),
            Some("failed")
        );
    }

    #[test]
    fn outcome_aggregate_verified_prefers_failed_then_passed_then_not_measured() {
        let v = |xs: &[&str]| xs.iter().map(|s| (*s).to_string()).collect::<Vec<_>>();
        assert_eq!(aggregate_verified(&v(&["passed", "failed"])), "failed");
        assert_eq!(
            aggregate_verified(&v(&["not_measured", "passed"])),
            "passed"
        );
        assert_eq!(aggregate_verified(&v(&["not_measured"])), "not_measured");
        assert_eq!(aggregate_verified(&[]), "not_measured");
    }

    #[tokio::test]
    async fn outcome_before_window_end_stays_pending() {
        let pool = pool().await;
        let id = pending_row(&pool, "D3", 1).await;
        finding(&pool, "D3-a", "D3", 1, 40_000).await;

        let counts = close_inject_outcomes(&pool, WINDOW_END_MS - 1)
            .await
            .unwrap();
        assert_eq!(counts, CloseCounts::default());
        let got = row(&pool, id).await;
        assert_eq!(got.outcome_state, "pending");
        assert_eq!(got.outcome, None);
        assert_eq!(got.resolved_at, None);
    }

    #[tokio::test]
    async fn outcome_void_when_the_finding_vanished_with_a_rule_version_change() {
        let pool = pool().await;
        let id = pending_row(&pool, "D3", 1).await;
        // The rule now runs as version 2: the finding that would have backed a hit is gone.
        finding(&pool, "D3-v2", "D3", 2, 80_000).await;

        let counts = close_inject_outcomes(&pool, WINDOW_END_MS).await.unwrap();
        assert_eq!(counts.void, 1);
        assert_eq!(counts.hit + counts.miss, 0);
        let got = row(&pool, id).await;
        assert_eq!(got.outcome_state, "void");
        assert_eq!(got.outcome, None);
    }

    #[tokio::test]
    async fn outcome_close_is_idempotent_and_skips_propose_rows() {
        let pool = pool().await;
        let id = pending_row(&pool, "D3", 1).await;
        finding(&pool, "D3-a", "D3", 1, 40_000).await;
        sqlx::query("INSERT INTO devtime_shadow (action, rule_id, project_id, session_id, subject_key, mode, decision, template_id, rule_version, action_version, thresholds_hash, rules_version, decided_at, window_end_at) VALUES ('propose', 'F1', 'p1', ?, 'F1-k', 'shadow', 'fire', 'propose.f1.v1', 1, 1, 'th', 'rv', '2026-10-05T10:00:00.000Z', '2026-10-05T10:30:00.000Z')")
            .bind(SID)
            .execute(&pool)
            .await
            .unwrap();

        let first = close_inject_outcomes(&pool, WINDOW_END_MS).await.unwrap();
        assert_eq!(first.hit, 1);
        let resolved = row(&pool, id).await;
        let second = close_inject_outcomes(&pool, WINDOW_END_MS + 5_000)
            .await
            .unwrap();
        assert_eq!(second, CloseCounts::default());
        assert_eq!(row(&pool, id).await, resolved);
        let propose_state: String =
            sqlx::query_scalar("SELECT outcome_state FROM devtime_shadow WHERE action = 'propose'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(propose_state, "pending");
    }

    // ---- 3.2 propose ------------------------------------------------------------------------
    //
    // Contract of `close_propose_outcomes` (rows with `action = 'propose'`, `outcome_state =
    // 'pending'`, `window_end_at <= ms_text(now_ms)`; inject rows and rows not yet due untouched):
    // - a human verdict prevails in every mode: `useful` -> `resolved`/`accepted`, `noise` ->
    //   `resolved`/`rejected`; the verdict itself is never touched;
    // - shadow, no verdict: a cross finding with `finding_key == subject_key` whose `ended_at` is
    //   inside (decided_at, window_end_at] -> `resolved`/`hit` (+ `outcome_finding_key`); else `miss`;
    // - live, no verdict: the row's `proposal_id` points at `proposals`: `approved` -> `accepted`,
    //   `rejected` -> `rejected`, still `pending` -> `ignored`; no `proposal_id` at all (no signal)
    //   -> state `expired`, no outcome;
    // - `resolved_at = ms_text(now_ms)`; closing is idempotent.

    /// `2026-10-19T10:30:00.000Z`: 14 days after the decision.
    const PROPOSE_END_MS: i64 = WINDOW_END_MS + 14 * 86_400_000;

    async fn propose_row(
        pool: &SqlitePool,
        key: &str,
        mode: &str,
        proposal_id: Option<i64>,
        human_verdict: Option<&str>,
    ) -> i64 {
        let row = st::ShadowRow {
            action: "propose".into(),
            rule_id: "F1".into(),
            project_id: "p1".into(),
            session_id: None,
            subject_key: key.into(),
            mode: mode.into(),
            decision: "fire".into(),
            template_id: "propose.f1.v1".into(),
            rule_version: 1,
            action_version: 1,
            thresholds_hash: "th".into(),
            rules_version: "rv".into(),
            decided_at: "2026-10-05T10:30:00.000Z".into(),
            window_end_at: "2026-10-19T10:30:00.000Z".into(),
            outcome_state: "pending".into(),
            proposal_id,
            human_verdict: human_verdict.map(Into::into),
            ..st::ShadowRow::default()
        };
        assert!(st::insert_decision(pool, &row).await.unwrap());
        sqlx::query_scalar(
            "SELECT id FROM devtime_shadow WHERE action = 'propose' AND subject_key = ?",
        )
        .bind(key)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    async fn cross_finding(pool: &SqlitePool, key: &str, ended_at: &str) {
        sqlx::query(
            "INSERT INTO devtime_findings (finding_key, project_id, session_id, scope, rule_id, \
             rule_version, level, waste, lever, confidence, lane, started_at, ended_at, cost_ms, \
             count, sessions, rules_version, parser_version) VALUES (?, 'p1', 's-x', 'cross', 'F1', \
             1, 'base', 'rework', 'x', 'exact', 'main', '2026-10-01T10:00:00.000Z', ?, 90000, 3, \
             '[]', 'rv', 1)",
        )
        .bind(key)
        .bind(ended_at)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn proposal(pool: &SqlitePool, status: &str) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO proposals (kind, status, project_id, reasoning, created_at) \
             VALUES ('devtime', ?, 'p1', 'r', '2026-10-05T10:30:00.000Z') RETURNING id",
        )
        .bind(status)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn propose_close_shadow_reappearance_in_the_window_is_a_hit() {
        let pool = pool().await;
        let id = propose_row(&pool, "F1-k", "shadow", None, None).await;
        cross_finding(&pool, "F1-k", "2026-10-12T09:00:00.000Z").await;

        let counts = close_propose_outcomes(&pool, PROPOSE_END_MS).await.unwrap();
        assert_eq!(
            counts,
            ProposeCloseCounts {
                hit: 1,
                ..ProposeCloseCounts::default()
            }
        );
        let got = row(&pool, id).await;
        assert_eq!(got.outcome_state, "resolved");
        assert_eq!(got.outcome.as_deref(), Some("hit"));
        assert_eq!(got.outcome_finding_key.as_deref(), Some("F1-k"));
        assert_eq!(
            got.resolved_at.as_deref(),
            Some(crate::devtime::actions::ms_text(PROPOSE_END_MS).as_str())
        );
    }

    #[tokio::test]
    async fn propose_close_shadow_without_reappearance_in_the_window_is_a_miss() {
        let pool = pool().await;
        let before = propose_row(&pool, "F1-old", "shadow", None, None).await;
        let after = propose_row(&pool, "F1-late", "shadow", None, None).await;
        // The finding that raised the proposal (before the decision) does not count, nor does one
        // that only recurs after the window.
        cross_finding(&pool, "F1-old", "2026-10-04T09:00:00.000Z").await;
        cross_finding(&pool, "F1-late", "2026-10-20T09:00:00.000Z").await;

        let counts = close_propose_outcomes(&pool, PROPOSE_END_MS).await.unwrap();
        assert_eq!(counts.miss, 2);
        assert_eq!(counts.hit, 0);
        for id in [before, after] {
            let got = row(&pool, id).await;
            assert_eq!(got.outcome_state, "resolved");
            assert_eq!(got.outcome.as_deref(), Some("miss"));
            assert_eq!(got.outcome_finding_key, None);
        }
    }

    #[tokio::test]
    async fn propose_close_human_mark_prevails_over_the_automatic_signal() {
        let pool = pool().await;
        // Reappeared (auto would be hit) but marked noise; did not reappear (auto miss) but useful.
        let noise = propose_row(&pool, "F1-n", "shadow", None, Some("noise")).await;
        let useful = propose_row(&pool, "F1-u", "shadow", None, Some("useful")).await;
        cross_finding(&pool, "F1-n", "2026-10-12T09:00:00.000Z").await;

        let counts = close_propose_outcomes(&pool, PROPOSE_END_MS).await.unwrap();
        assert_eq!(counts.rejected, 1);
        assert_eq!(counts.accepted, 1);
        assert_eq!(counts.hit + counts.miss, 0);
        let n = row(&pool, noise).await;
        assert_eq!(n.outcome_state, "resolved");
        assert_eq!(n.outcome.as_deref(), Some("rejected"));
        assert_eq!(n.human_verdict.as_deref(), Some("noise"));
        let u = row(&pool, useful).await;
        assert_eq!(u.outcome.as_deref(), Some("accepted"));
        assert_eq!(u.human_verdict.as_deref(), Some("useful"));
    }

    #[tokio::test]
    async fn propose_close_live_maps_the_proposal_status() {
        let pool = pool().await;
        let ok = proposal(&pool, "approved").await;
        let no = proposal(&pool, "rejected").await;
        let open = proposal(&pool, "pending").await;
        let a = propose_row(&pool, "F1-a", "live", Some(ok), None).await;
        let r = propose_row(&pool, "F1-r", "live", Some(no), None).await;
        let i = propose_row(&pool, "F1-i", "live", Some(open), None).await;

        let counts = close_propose_outcomes(&pool, PROPOSE_END_MS).await.unwrap();
        assert_eq!(
            counts,
            ProposeCloseCounts {
                accepted: 1,
                rejected: 1,
                ignored: 1,
                ..ProposeCloseCounts::default()
            }
        );
        for (id, want) in [(a, "accepted"), (r, "rejected"), (i, "ignored")] {
            let got = row(&pool, id).await;
            assert_eq!(got.outcome_state, "resolved");
            assert_eq!(got.outcome.as_deref(), Some(want));
        }
        assert_eq!(row(&pool, a).await.proposal_id, Some(ok));
    }

    #[tokio::test]
    async fn propose_close_live_without_any_signal_expires() {
        let pool = pool().await;
        let id = propose_row(&pool, "F1-none", "live", None, None).await;

        let counts = close_propose_outcomes(&pool, PROPOSE_END_MS).await.unwrap();
        assert_eq!(
            counts,
            ProposeCloseCounts {
                expired: 1,
                ..ProposeCloseCounts::default()
            }
        );
        let got = row(&pool, id).await;
        assert_eq!(got.outcome_state, "expired");
        assert_eq!(got.outcome, None);
        assert!(got.resolved_at.is_some());
    }

    #[tokio::test]
    async fn propose_close_leaves_not_due_and_inject_rows_and_is_idempotent() {
        let pool = pool().await;
        let id = propose_row(&pool, "F1-k", "shadow", None, None).await;
        let inject = pending_row(&pool, "D3", 1).await;
        cross_finding(&pool, "F1-k", "2026-10-12T09:00:00.000Z").await;

        let early = close_propose_outcomes(&pool, PROPOSE_END_MS - 1)
            .await
            .unwrap();
        assert_eq!(early, ProposeCloseCounts::default());
        assert_eq!(row(&pool, id).await.outcome_state, "pending");

        let first = close_propose_outcomes(&pool, PROPOSE_END_MS).await.unwrap();
        assert_eq!(first.hit, 1);
        let resolved = row(&pool, id).await;
        let second = close_propose_outcomes(&pool, PROPOSE_END_MS + 5_000)
            .await
            .unwrap();
        assert_eq!(second, ProposeCloseCounts::default());
        assert_eq!(row(&pool, id).await, resolved);
        // The inject row is due too, but belongs to close_inject_outcomes.
        assert_eq!(row(&pool, inject).await.outcome_state, "pending");
    }
}
