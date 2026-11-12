//! Live delivery of the automatic actions (actions design spec, section 7 step 5).
//!
//! `propose`: a `live` `fire` row of `devtime_shadow` with no proposal yet becomes one row of
//! `proposals` (`kind` `devtime-script` for F1, `devtime-gotcha` for F2), and the shadow row keeps
//! its id in `proposal_id`, so `close_propose_outcomes` reads the owner's answer from it.
//!
//! `inject`: [`inject_deliveries`] returns the lines the sub-project 4 summary surface prints at
//! session start, and marks each row delivered (`delivered_ms` in `params`) so one decision is shown
//! once. Nothing here is free text: a line is a closed template filled with counts and minutes.
//!
//! Shadow rows are never delivered, and a pair the owner (or the rollback) moved back out of `live`
//! delivers nothing even if an older `live` row is still pending.

use sqlx::SqlitePool;

use crate::config::DevtimeActionsConfig;

/// `proposals.kind` of a `propose` rule, `None` when the rule has no proposal kind.
pub fn proposal_kind(rule_id: &str) -> Option<&'static str> {
    match rule_id {
        "F1" => Some("devtime-script"),
        "F2" => Some("devtime-gotcha"),
        _ => None,
    }
}

/// PURE: the owner-facing reason of a proposal, from the closed template and two numbers.
pub fn proposal_reasoning(template_id: &str, count: i64, cost_ms: i64) -> Option<String> {
    let minutes = cost_ms / 60_000;
    match template_id {
        "propose.f1.v1" => Some(format!(
            "The same command sequence recurred {count} times across sessions, about {minutes} min in total. A script would save it."
        )),
        "propose.f2.v1" => Some(format!(
            "The same failure recurred {count} times across sessions, about {minutes} min in total. A gotcha note would save it."
        )),
        _ => None,
    }
}

/// PURE: the one summary line of an `inject` decision, from the closed template and two numbers.
pub fn inject_line(
    template_id: &str,
    count: i64,
    cost_ms: i64,
    window_days: u32,
) -> Option<String> {
    let minutes = cost_ms / 60_000;
    match template_id {
        "inject.d3.v1" => Some(format!(
            "Devtime: {count} search bursts in the last {window_days} days here cost about {minutes} min; delegate wide searches to a subagent."
        )),
        "inject.d6.v1" => Some(format!(
            "Devtime: {count} long foreground commands in the last {window_days} days here cost about {minutes} min; run them in the background."
        )),
        _ => None,
    }
}

/// One line for the summary surface, with the shadow row it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct InjectDelivery {
    pub shadow_id: i64,
    pub rule_id: String,
    pub line: String,
}

/// Is the pair currently `live`? A missing row means `shadow`.
async fn pair_is_live(pool: &SqlitePool, action: &str, rule_id: &str) -> sqlx::Result<bool> {
    Ok(
        crate::devtime::store::get_action_state(pool, action, rule_id)
            .await?
            .is_some_and(|row| row.mode == "live"),
    )
}

/// Creates the `proposals` row of every `live` `propose` `fire` that has none. Returns how many.
/// Idempotent: a row with a `proposal_id` is skipped, so a second run creates nothing.
pub async fn deliver_proposals(
    pool: &SqlitePool,
    cfg: &DevtimeActionsConfig,
    now_ms: i64,
) -> sqlx::Result<usize> {
    if !cfg.enabled {
        return Ok(0);
    }
    let rows: Vec<(i64, String, String, String, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT id, rule_id, project_id, template_id, basis_recent_count, basis_recent_cost_ms \
         FROM devtime_shadow WHERE action = 'propose' AND mode = 'live' AND decision = 'fire' \
         AND outcome_state = 'pending' AND proposal_id IS NULL ORDER BY id",
    )
    .fetch_all(pool)
    .await?;
    let created_at = super::ms_text(now_ms);
    let mut made = 0_usize;
    for (id, rule_id, project_id, template_id, count, cost_ms) in rows {
        let (Some(kind), Some(reasoning)) = (
            proposal_kind(&rule_id),
            proposal_reasoning(&template_id, count.unwrap_or(0), cost_ms.unwrap_or(0)),
        ) else {
            continue;
        };
        if !pair_is_live(pool, "propose", &rule_id).await? {
            continue;
        }
        let mut tx = pool.begin().await?;
        let proposal_id: i64 = sqlx::query_scalar(
            "INSERT INTO proposals (kind, status, project_id, reasoning, created_at) \
             VALUES (?, 'pending', ?, ?, ?) RETURNING id",
        )
        .bind(kind)
        .bind(&project_id)
        .bind(&reasoning)
        .bind(&created_at)
        .fetch_one(&mut *tx)
        .await?;
        let linked = sqlx::query(
            "UPDATE devtime_shadow SET proposal_id = ? WHERE id = ? AND proposal_id IS NULL",
        )
        .bind(proposal_id)
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        if linked == 1 {
            tx.commit().await?;
            made += 1;
        } else {
            tx.rollback().await?;
        }
    }
    Ok(made)
}

/// The `inject` lines to show at the start of a session in `project_id`: the `live` `fire` rows of
/// the project not yet delivered, decided inside the last `inject_window_days`, costliest first, at
/// most `inject_max_per_session`. Each returned row is marked delivered, so a second call is empty.
pub async fn inject_deliveries(
    pool: &SqlitePool,
    cfg: &DevtimeActionsConfig,
    project_id: &str,
    now_ms: i64,
) -> sqlx::Result<Vec<InjectDelivery>> {
    if !cfg.enabled {
        return Ok(Vec::new());
    }
    let since = super::ms_text(
        now_ms.saturating_sub(i64::from(cfg.inject_window_days).saturating_mul(86_400_000)),
    );
    let rows: Vec<(i64, String, String, Option<i64>, Option<i64>)> = sqlx::query_as(
        "SELECT id, rule_id, template_id, basis_recent_count, basis_recent_cost_ms \
         FROM devtime_shadow WHERE action = 'inject' AND mode = 'live' AND decision = 'fire' \
         AND outcome_state = 'pending' AND project_id = ? AND decided_at >= ? \
         AND json_extract(params, '$.delivered_ms') IS NULL \
         ORDER BY COALESCE(basis_recent_cost_ms, 0) DESC, rule_id ASC, id ASC",
    )
    .bind(project_id)
    .bind(&since)
    .fetch_all(pool)
    .await?;
    let keep = usize::try_from(cfg.inject_max_per_session).unwrap_or(usize::MAX);
    let mut out = Vec::new();
    for (id, rule_id, template_id, count, cost_ms) in rows {
        if out.len() >= keep {
            break;
        }
        let Some(line) = inject_line(
            &template_id,
            count.unwrap_or(0),
            cost_ms.unwrap_or(0),
            cfg.inject_window_days,
        ) else {
            continue;
        };
        if !pair_is_live(pool, "inject", &rule_id).await? {
            continue;
        }
        let marked = sqlx::query(
            "UPDATE devtime_shadow SET params = json_set(params, '$.delivered_ms', ?) \
             WHERE id = ? AND json_extract(params, '$.delivered_ms') IS NULL",
        )
        .bind(now_ms)
        .bind(id)
        .execute(pool)
        .await?
        .rows_affected();
        if marked == 1 {
            out.push(InjectDelivery {
                shadow_id: id,
                rule_id,
                line,
            });
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devtime::store as st;

    const NOW_MS: i64 = 1_791_196_200_000;

    fn cfg() -> DevtimeActionsConfig {
        crate::config::DevtimeConfig::default().actions
    }

    async fn set_mode(pool: &SqlitePool, action: &str, rule: &str, mode: &str) {
        st::set_action_state(
            pool,
            &st::ActionStateRow {
                action: action.into(),
                rule_id: rule.into(),
                mode: mode.into(),
                since: super::super::ms_text(NOW_MS),
                changed_by: "owner".into(),
                reason: None,
            },
        )
        .await
        .unwrap();
    }

    async fn shadow(
        pool: &SqlitePool,
        action: &str,
        rule: &str,
        mode: &str,
        decision: &str,
        key: &str,
        cost_ms: i64,
    ) -> i64 {
        let template = match (action, rule) {
            ("inject", "D3") => "inject.d3.v1",
            ("inject", _) => "inject.d6.v1",
            (_, "F1") => "propose.f1.v1",
            _ => "propose.f2.v1",
        };
        st::insert_decision(
            pool,
            &st::ShadowRow {
                action: action.into(),
                rule_id: rule.into(),
                project_id: "p1".into(),
                session_id: (action == "inject").then(|| "s1".to_string()),
                subject_key: key.into(),
                mode: mode.into(),
                decision: decision.into(),
                suppress_reason: (decision == "suppress").then(|| "cap".to_string()),
                template_id: template.into(),
                basis_recent_count: Some(4),
                basis_recent_cost_ms: Some(cost_ms),
                rule_version: 1,
                action_version: 1,
                thresholds_hash: "h".into(),
                rules_version: "rv".into(),
                decided_at: super::super::ms_text(NOW_MS - 1_000),
                window_end_at: super::super::ms_text(NOW_MS + 86_400_000),
                outcome_state: "pending".into(),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        sqlx::query_scalar("SELECT id FROM devtime_shadow WHERE subject_key = ? AND mode = ?")
            .bind(key)
            .bind(mode)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    async fn proposal_count(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM proposals")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn live_propose_fire_creates_one_proposal_and_links_it() {
        let pool = crate::testdb::fresh_pool().await;
        set_mode(&pool, "propose", "F1", "live").await;
        let id = shadow(&pool, "propose", "F1", "live", "fire", "F1-k", 600_000).await;

        assert_eq!(deliver_proposals(&pool, &cfg(), NOW_MS).await.unwrap(), 1);
        let (kind, status, project, reasoning): (String, String, String, String) =
            sqlx::query_as("SELECT kind, status, project_id, reasoning FROM proposals")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(kind, "devtime-script");
        assert_eq!(status, "pending");
        assert_eq!(project, "p1");
        assert!(reasoning.contains("4 times") && reasoning.contains("10 min"));
        let linked: Option<i64> =
            sqlx::query_scalar("SELECT proposal_id FROM devtime_shadow WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert!(linked.is_some());

        // A second pass creates nothing.
        assert_eq!(deliver_proposals(&pool, &cfg(), NOW_MS).await.unwrap(), 0);
        assert_eq!(proposal_count(&pool).await, 1);
    }

    #[tokio::test]
    async fn f2_proposal_is_a_gotcha() {
        let pool = crate::testdb::fresh_pool().await;
        set_mode(&pool, "propose", "F2", "live").await;
        shadow(&pool, "propose", "F2", "live", "fire", "F2-k", 300_000).await;
        deliver_proposals(&pool, &cfg(), NOW_MS).await.unwrap();
        let kind: String = sqlx::query_scalar("SELECT kind FROM proposals")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(kind, "devtime-gotcha");
    }

    #[tokio::test]
    async fn shadow_suppressed_and_demoted_rows_create_no_proposal() {
        let pool = crate::testdb::fresh_pool().await;
        set_mode(&pool, "propose", "F1", "live").await;
        shadow(
            &pool, "propose", "F1", "shadow", "fire", "k-shadow", 600_000,
        )
        .await;
        shadow(
            &pool, "propose", "F1", "live", "suppress", "k-supp", 600_000,
        )
        .await;
        // F2 has a pending live row, but the pair was rolled back to shadow.
        set_mode(&pool, "propose", "F2", "shadow").await;
        shadow(&pool, "propose", "F2", "live", "fire", "k-demoted", 600_000).await;

        assert_eq!(deliver_proposals(&pool, &cfg(), NOW_MS).await.unwrap(), 0);
        assert_eq!(proposal_count(&pool).await, 0);
    }

    #[tokio::test]
    async fn disabled_actions_deliver_nothing() {
        let pool = crate::testdb::fresh_pool().await;
        set_mode(&pool, "propose", "F1", "live").await;
        shadow(&pool, "propose", "F1", "live", "fire", "F1-k", 600_000).await;
        let mut off = cfg();
        off.enabled = false;
        assert_eq!(deliver_proposals(&pool, &off, NOW_MS).await.unwrap(), 0);
        assert!(
            inject_deliveries(&pool, &off, "p1", NOW_MS)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn delivered_proposal_closes_through_its_status() {
        // The link written here is what `close_propose_outcomes` reads for a live row.
        let pool = crate::testdb::fresh_pool().await;
        set_mode(&pool, "propose", "F1", "live").await;
        let id = shadow(&pool, "propose", "F1", "live", "fire", "F1-k", 600_000).await;
        deliver_proposals(&pool, &cfg(), NOW_MS).await.unwrap();
        sqlx::query("UPDATE proposals SET status = 'approved'")
            .execute(&pool)
            .await
            .unwrap();
        let end = NOW_MS + 90_000_000;
        super::super::outcome::close_propose_outcomes(&pool, end)
            .await
            .unwrap();
        let outcome: Option<String> =
            sqlx::query_scalar("SELECT outcome FROM devtime_shadow WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(outcome.as_deref(), Some("accepted"));
    }

    #[tokio::test]
    async fn inject_lines_are_live_fire_costliest_first_and_shown_once() {
        let pool = crate::testdb::fresh_pool().await;
        set_mode(&pool, "inject", "D3", "live").await;
        set_mode(&pool, "inject", "D6", "live").await;
        shadow(&pool, "inject", "D3", "live", "fire", "D3|p1", 180_000).await;
        shadow(&pool, "inject", "D6", "live", "fire", "D6|p1", 600_000).await;

        let got = inject_deliveries(&pool, &cfg(), "p1", NOW_MS)
            .await
            .unwrap();
        let rules: Vec<&str> = got.iter().map(|d| d.rule_id.as_str()).collect();
        assert_eq!(rules, ["D6", "D3"]);
        assert!(got[0].line.contains("10 min") && got[0].line.contains("background"));
        assert!(got[1].line.contains("3 min") && got[1].line.contains("subagent"));

        assert!(
            inject_deliveries(&pool, &cfg(), "p1", NOW_MS)
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn inject_skips_shadow_suppressed_other_projects_and_demoted_pairs() {
        let pool = crate::testdb::fresh_pool().await;
        set_mode(&pool, "inject", "D3", "live").await;
        set_mode(&pool, "inject", "D6", "shadow").await;
        shadow(&pool, "inject", "D3", "shadow", "fire", "a", 180_000).await;
        shadow(&pool, "inject", "D3", "live", "suppress", "b", 180_000).await;
        shadow(&pool, "inject", "D6", "live", "fire", "c", 180_000).await;
        shadow(&pool, "inject", "D3", "live", "fire", "d", 180_000).await;

        assert!(
            inject_deliveries(&pool, &cfg(), "other", NOW_MS)
                .await
                .unwrap()
                .is_empty()
        );
        let got = inject_deliveries(&pool, &cfg(), "p1", NOW_MS)
            .await
            .unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].rule_id, "D3");
    }

    #[tokio::test]
    async fn inject_respects_the_per_session_cap() {
        let pool = crate::testdb::fresh_pool().await;
        set_mode(&pool, "inject", "D3", "live").await;
        set_mode(&pool, "inject", "D6", "live").await;
        shadow(&pool, "inject", "D3", "live", "fire", "x", 180_000).await;
        shadow(&pool, "inject", "D6", "live", "fire", "y", 600_000).await;
        let mut one = cfg();
        one.inject_max_per_session = 1;
        let got = inject_deliveries(&pool, &one, "p1", NOW_MS).await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].rule_id, "D6");
    }

    #[test]
    fn templates_are_closed() {
        assert!(proposal_reasoning("propose.f1.v1", 3, 120_000).is_some());
        assert!(proposal_reasoning("inject.d3.v1", 3, 120_000).is_none());
        assert!(inject_line("propose.f1.v1", 3, 120_000, 14).is_none());
        assert_eq!(proposal_kind("D3"), None);
    }
}
