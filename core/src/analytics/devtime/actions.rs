//! The automatic actions' owner-facing reads and writes (actions design spec; plan chunk 3.4): the
//! per-pair report with its state, the "what the system would have said" list of shadow decisions,
//! the owner's `useful` / `noise` mark on one of them, and the manual shadow-to-live promotion (D2).
//! The HTTP layer (`http/analytics.rs`) only parses and maps [`ActionError`] to status codes.
//!
//! Contract (the GREEN step implements exactly this; where the plan was ambiguous the choice below
//! is the one the tests pin):
//! - [`action_pairs`]: one [`ActionPair`] per `(action, rule)` of `cfg.actions.pairs` (map order:
//!   `inject` then `propose`, rules in the listed order). `mode` is the EFFECTIVE mode, as
//!   `actions::pair_mode` reads it: the stored row's, and `shadow` when the pair has no row (a pair
//!   without a row runs in shadow); `since` / `changed_by` are `None` without a row. `report` is
//!   `action_promotion_report` over all projects at the rule's current version and
//!   `ACTION_VERSION`, with the precision from `precision::rule_report` (all time). A pair with no
//!   shadow rows gets an all-zero report (never an error). The report's figures are the whole
//!   answer: no free text, no ids.
//! - [`shadow_list`]: rows with `mode = 'shadow'` (fire and suppress alike, any outcome state but
//!   `void`), optionally filtered by `action` (must be in `ACTIONS`, else
//!   [`ActionError::BadAction`]) and `rule_id`, newest first (`decided_at` desc, `id` desc),
//!   `limit` rows and a `next_cursor` when more exist. A [`ShadowItem`] carries the closed fields
//!   only: never `session_id`, `subject_key`, a path or any free text; `params` is the stored JSON
//!   object parsed (`{}` when it does not parse).
//! - [`mark_shadow`]: sets the human verdict on a row that is `mode = 'shadow'` and
//!   `decision = 'fire'`; any other row, or no row, is [`ActionError::UnknownShadow`]; a verdict
//!   outside `HUMAN_VERDICTS` is [`ActionError::BadVerdict`] (checked first). Marking again replaces
//!   it; the same mark twice is the same state. Answers the updated item.
//! - [`promote_pair`]: `action` outside `ACTIONS` is `BadAction`; a rule that `cfg.actions.pairs`
//!   does not list under that action is `UnknownRule`; a report with `ready == false` is `NotReady`
//!   and writes nothing; otherwise writes `devtime_action_state` with `mode = 'live'`,
//!   `changed_by = 'owner'` and answers the pair as [`action_pairs`] would after the write.

use serde::Serialize;
use sqlx::SqlitePool;

use crate::config::DevtimeConfig;
use crate::devtime::actions::ACTION_VERSION;
use crate::devtime::actions::report::{PromotionReport, PromotionScope, action_promotion_report};
use crate::devtime::precision::{RulePrecision, rule_report};
use crate::devtime::store::{self, ACTIONS, HUMAN_VERDICTS};

/// Why a read or write of this module refused. The HTTP layer maps the first five to a JSON code.
#[derive(Debug)]
pub enum ActionError {
    BadAction,
    BadVerdict,
    UnknownRule,
    UnknownShadow,
    NotReady,
    Db(sqlx::Error),
}

impl From<sqlx::Error> for ActionError {
    fn from(error: sqlx::Error) -> Self {
        ActionError::Db(error)
    }
}

/// One `(action, rule)` pair with its mode and its promotion report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActionPair {
    pub action: String,
    pub rule_id: String,
    pub mode: String,
    pub since: Option<String>,
    pub changed_by: Option<String>,
    pub report: PromotionReport,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActionPairs {
    pub pairs: Vec<ActionPair>,
}

/// One shadow decision as the owner sees it: the closed fields only.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ShadowItem {
    pub id: i64,
    pub action: String,
    pub rule_id: String,
    pub project_id: String,
    pub decision: String,
    pub suppress_reason: Option<String>,
    pub template_id: String,
    pub params: serde_json::Value,
    pub decided_at: String,
    pub outcome_state: String,
    pub outcome: Option<String>,
    pub human_verdict: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ShadowPage {
    pub items: Vec<ShadowItem>,
    pub next_cursor: Option<String>,
}

/// The position after which the next page starts: the last item's `(decided_at, id)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShadowCursor {
    pub decided_at: String,
    pub id: i64,
}

#[derive(Debug, Clone)]
pub struct ShadowQuery<'a> {
    pub action: Option<&'a str>,
    pub rule_id: Option<&'a str>,
    /// 1..=200; the HTTP layer enforces the range.
    pub limit: i64,
    pub cursor: Option<ShadowCursor>,
}

/// Opaque text for [`ShadowCursor`]; [`decode_shadow_cursor`] reads back exactly what this wrote.
/// Hex of `id|decided_at`, so it is URL-safe and says nothing to the eye.
pub fn encode_shadow_cursor(cursor: &ShadowCursor) -> String {
    format!("{}|{}", cursor.id, cursor.decided_at)
        .bytes()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// `None` for anything [`encode_shadow_cursor`] did not write.
pub fn decode_shadow_cursor(raw: &str) -> Option<ShadowCursor> {
    if raw.is_empty() || !raw.len().is_multiple_of(2) || !raw.is_ascii() {
        return None;
    }
    let bytes: Option<Vec<u8>> = (0..raw.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&raw[i..i + 2], 16).ok())
        .collect();
    let text = String::from_utf8(bytes?).ok()?;
    let (id, decided_at) = text.split_once('|')?;
    chrono::DateTime::parse_from_rfc3339(decided_at).ok()?;
    Some(ShadowCursor {
        decided_at: decided_at.to_string(),
        id: id.parse().ok()?,
    })
}

/// Unix milliseconds to the `%Y-%m-%dT%H:%M:%S%.3fZ` text the devtime tables store.
fn ms_text(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

/// One pair's view, or `None` when the rule is not in the registry (nothing to report on).
async fn pair_view(
    pool: &SqlitePool,
    cfg: &DevtimeConfig,
    precisions: &[RulePrecision],
    action: &str,
    rule_id: &str,
    now_ms: i64,
) -> sqlx::Result<Option<ActionPair>> {
    let Some(rule) = crate::devtime::rules::rule(rule_id) else {
        return Ok(None);
    };
    let Some(precision) = precisions.iter().find(|p| p.rule_id == rule_id) else {
        return Ok(None);
    };
    let state = store::get_action_state(pool, action, rule_id).await?;
    let report = action_promotion_report(
        pool,
        &cfg.actions,
        precision,
        &PromotionScope {
            action,
            rule_id,
            project_id: None,
            rule_version: i64::from(rule.version),
            action_version: ACTION_VERSION,
            now_ms,
        },
    )
    .await?;
    // No stored row: the pair runs in shadow (`actions::pair_mode`).
    let (mode, since, changed_by) = match state {
        Some(row) => (row.mode, Some(row.since), Some(row.changed_by)),
        None => ("shadow".to_string(), None, None),
    };
    Ok(Some(ActionPair {
        action: action.to_string(),
        rule_id: rule_id.to_string(),
        mode,
        since,
        changed_by,
        report,
    }))
}

/// Every configured pair with its state and report. See the module contract.
pub async fn action_pairs(
    pool: &SqlitePool,
    cfg: &DevtimeConfig,
    now_ms: i64,
) -> sqlx::Result<ActionPairs> {
    let precisions = rule_report(pool, &cfg.rules, None).await?;
    let mut pairs = Vec::new();
    for (action, rules) in &cfg.actions.pairs {
        if !ACTIONS.contains(&action.as_str()) {
            continue;
        }
        for rule_id in rules {
            if let Some(pair) = pair_view(pool, cfg, &precisions, action, rule_id, now_ms).await? {
                pairs.push(pair);
            }
        }
    }
    Ok(ActionPairs { pairs })
}

fn item_of(row: store::ShadowRow) -> ShadowItem {
    let params = serde_json::from_str::<serde_json::Value>(&row.params)
        .ok()
        .filter(serde_json::Value::is_object)
        .unwrap_or_else(|| serde_json::json!({}));
    ShadowItem {
        id: row.id,
        action: row.action,
        rule_id: row.rule_id,
        project_id: row.project_id,
        decision: row.decision,
        suppress_reason: row.suppress_reason,
        template_id: row.template_id,
        params,
        decided_at: row.decided_at,
        outcome_state: row.outcome_state,
        outcome: row.outcome,
        human_verdict: row.human_verdict,
    }
}

/// A page of the shadow decisions. See the module contract.
pub async fn shadow_list(
    pool: &SqlitePool,
    query: &ShadowQuery<'_>,
) -> Result<ShadowPage, ActionError> {
    if query.action.is_some_and(|a| !ACTIONS.contains(&a)) {
        return Err(ActionError::BadAction);
    }
    let limit = query.limit.max(1);
    let (after_at, after_id) = match &query.cursor {
        Some(c) => (Some(c.decided_at.as_str()), Some(c.id)),
        None => (None, None),
    };
    let mut rows = sqlx::query_as::<_, store::ShadowRow>(
        "SELECT * FROM devtime_shadow WHERE mode = 'shadow' AND outcome_state != 'void' \
         AND (?1 IS NULL OR action = ?1) AND (?2 IS NULL OR rule_id = ?2) \
         AND (?3 IS NULL OR decided_at < ?3 OR (decided_at = ?3 AND id < ?4)) \
         ORDER BY decided_at DESC, id DESC LIMIT ?5",
    )
    .bind(query.action)
    .bind(query.rule_id)
    .bind(after_at)
    .bind(after_id)
    .bind(limit.saturating_add(1))
    .fetch_all(pool)
    .await?;
    let more = i64::try_from(rows.len()).unwrap_or(i64::MAX) > limit;
    rows.truncate(usize::try_from(limit).unwrap_or(usize::MAX));
    let next_cursor = match rows.last() {
        Some(last) if more => Some(encode_shadow_cursor(&ShadowCursor {
            decided_at: last.decided_at.clone(),
            id: last.id,
        })),
        _ => None,
    };
    Ok(ShadowPage {
        items: rows.into_iter().map(item_of).collect(),
        next_cursor,
    })
}

async fn shadow_row_by_id(pool: &SqlitePool, id: i64) -> sqlx::Result<Option<store::ShadowRow>> {
    sqlx::query_as::<_, store::ShadowRow>("SELECT * FROM devtime_shadow WHERE id = ?")
        .bind(id)
        .fetch_optional(pool)
        .await
}

/// The owner's `useful` / `noise` mark on one shadow decision. See the module contract.
pub async fn mark_shadow(
    pool: &SqlitePool,
    id: i64,
    verdict: &str,
) -> Result<ShadowItem, ActionError> {
    if !HUMAN_VERDICTS.contains(&verdict) {
        return Err(ActionError::BadVerdict);
    }
    match shadow_row_by_id(pool, id).await? {
        Some(row) if row.mode == "shadow" && row.decision == "fire" => {}
        _ => return Err(ActionError::UnknownShadow),
    }
    if !store::set_human_verdict(pool, id, verdict).await? {
        return Err(ActionError::UnknownShadow);
    }
    shadow_row_by_id(pool, id)
        .await?
        .map(item_of)
        .ok_or(ActionError::UnknownShadow)
}

/// The owner's manual promotion of a pair to `live`. See the module contract.
pub async fn promote_pair(
    pool: &SqlitePool,
    cfg: &DevtimeConfig,
    action: &str,
    rule_id: &str,
    now_ms: i64,
) -> Result<ActionPair, ActionError> {
    if !ACTIONS.contains(&action) {
        return Err(ActionError::BadAction);
    }
    if !cfg
        .actions
        .pairs
        .get(action)
        .is_some_and(|rules| rules.iter().any(|r| r == rule_id))
    {
        return Err(ActionError::UnknownRule);
    }
    let precisions = rule_report(pool, &cfg.rules, None).await?;
    let before = pair_view(pool, cfg, &precisions, action, rule_id, now_ms)
        .await?
        .ok_or(ActionError::UnknownRule)?;
    if !before.report.ready {
        return Err(ActionError::NotReady);
    }
    store::set_action_state(
        pool,
        &store::ActionStateRow {
            action: action.to_string(),
            rule_id: rule_id.to_string(),
            mode: "live".to_string(),
            since: ms_text(now_ms),
            changed_by: "owner".to_string(),
            reason: None,
        },
    )
    .await?;
    pair_view(pool, cfg, &precisions, action, rule_id, now_ms)
        .await?
        .ok_or(ActionError::UnknownRule)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devtime::actions::ACTION_VERSION;
    use crate::devtime::store as st;

    /// `2026-10-05T10:30:00.000Z`.
    const NOW_MS: i64 = 1_791_196_200_000;
    const DAY_MS: i64 = 86_400_000;
    const HOUR_MS: i64 = 3_600_000;

    async fn pool() -> SqlitePool {
        crate::testdb::fresh_pool().await
    }

    fn ts(ms: i64) -> String {
        chrono::DateTime::from_timestamp_millis(ms)
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string()
    }

    fn rule_version(rule: &str) -> i64 {
        i64::from(crate::devtime::rules::rule(rule).unwrap().version)
    }

    /// A resolved shadow `fire` row of D3 in `p1`; `edit` changes it before it is stored.
    async fn shadow_row(
        pool: &SqlitePool,
        tag: &str,
        session: &str,
        decided_ms: i64,
        edit: impl FnOnce(&mut st::ShadowRow),
    ) -> i64 {
        let mut row = st::ShadowRow {
            action: "inject".into(),
            rule_id: "D3".into(),
            project_id: "p1".into(),
            session_id: Some(session.into()),
            subject_key: format!("D3|p1|{tag}"),
            mode: "shadow".into(),
            decision: "fire".into(),
            template_id: "inject.d3.v1".into(),
            params: r#"{"count":3}"#.into(),
            rule_version: rule_version("D3"),
            action_version: ACTION_VERSION,
            thresholds_hash: "th".into(),
            rules_version: "rv".into(),
            decided_at: ts(decided_ms),
            window_end_at: ts(decided_ms + HOUR_MS / 2),
            outcome_state: "resolved".into(),
            outcome: Some("miss".into()),
            outcome_cost_ms: Some(0),
            resolved_at: Some(ts(decided_ms + HOUR_MS)),
            ..st::ShadowRow::default()
        };
        edit(&mut row);
        assert!(st::insert_decision(pool, &row).await.unwrap());
        sqlx::query_scalar("SELECT id FROM devtime_shadow WHERE subject_key = ? AND mode = ?")
            .bind(&row.subject_key)
            .bind(&row.mode)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// A pair whose report is `ready`: 30 resolved shadow fires over 12 sessions starting 20 days
    /// ago (24 hits of 60 s, 6 `useful` marks), and 22 `exact` D3 findings none of them marked
    /// `not_rework` (Wilson lower ~0.85, above the 0.6 of D1). No turn stats, so the avoidable
    /// fraction has no baseline and cannot regress.
    async fn seed_ready_d3(pool: &SqlitePool) {
        for i in 0..22 {
            sqlx::query(
                "INSERT INTO devtime_findings (finding_key, project_id, session_id, scope, \
                 rule_id, rule_version, level, waste, lever, confidence, lane, started_at, \
                 ended_at, cost_ms, count, sessions, rules_version, parser_version) \
                 VALUES (?, 'p1', 's0', 'session', 'D3', ?, 'base', 'rework', 'x', 'exact', \
                 'main', ?, ?, 1000, 1, '[]', 'rv', 1)",
            )
            .bind(format!("D3-f{i}"))
            .bind(rule_version("D3"))
            .bind(ts(NOW_MS - 40 * DAY_MS))
            .bind(ts(NOW_MS - 40 * DAY_MS + 60_000))
            .execute(pool)
            .await
            .unwrap();
        }
        let mut ids = Vec::new();
        for i in 0..30_i64 {
            let hit = i < 24;
            let id = shadow_row(
                pool,
                &i.to_string(),
                &format!("s{}", i % 12),
                NOW_MS - 20 * DAY_MS + i * HOUR_MS,
                |r| {
                    if hit {
                        r.outcome = Some("hit".into());
                        r.outcome_cost_ms = Some(60_000);
                        r.outcome_finding_key = Some(format!("D3-{i}"));
                    }
                },
            )
            .await;
            ids.push(id);
        }
        for id in ids.iter().take(6) {
            assert!(st::set_human_verdict(pool, *id, "useful").await.unwrap());
        }
    }

    #[tokio::test]
    async fn action_pairs_lists_every_configured_pair_with_its_state_and_a_zeroed_report() {
        let pool = pool().await;
        st::set_action_state(
            &pool,
            &st::ActionStateRow {
                action: "inject".into(),
                rule_id: "D3".into(),
                mode: "live".into(),
                since: "2026-10-01T00:00:00.000Z".into(),
                changed_by: "owner".into(),
                reason: None,
            },
        )
        .await
        .unwrap();
        let got = action_pairs(&pool, &DevtimeConfig::default(), NOW_MS)
            .await
            .unwrap();
        let keys: Vec<_> = got
            .pairs
            .iter()
            .map(|p| (p.action.as_str(), p.rule_id.as_str()))
            .collect();
        assert_eq!(
            keys,
            [
                ("inject", "D3"),
                ("inject", "D6"),
                ("propose", "F1"),
                ("propose", "F2")
            ]
        );
        let d3 = &got.pairs[0];
        assert_eq!(d3.mode, "live");
        assert_eq!(d3.since.as_deref(), Some("2026-10-01T00:00:00.000Z"));
        assert_eq!(d3.changed_by.as_deref(), Some("owner"));
        let d6 = &got.pairs[1];
        // No stored row: the pair runs in shadow, with no `since` and no `changed_by`.
        assert_eq!(d6.mode, "shadow");
        assert!(d6.since.is_none() && d6.changed_by.is_none());
        for pair in &got.pairs {
            assert_eq!(pair.report.n_resolved, 0, "{pair:?}");
            assert!(
                pair.report.hit_rate.is_none() && !pair.report.ready,
                "{pair:?}"
            );
        }
    }

    #[tokio::test]
    async fn action_pairs_reports_a_ready_pair_as_ready() {
        let pool = pool().await;
        seed_ready_d3(&pool).await;
        let got = action_pairs(&pool, &DevtimeConfig::default(), NOW_MS)
            .await
            .unwrap();
        let d3 = got.pairs.iter().find(|p| p.rule_id == "D3").unwrap();
        assert_eq!(d3.report.n_resolved, 30);
        assert!(d3.report.ready, "{:?}", d3.report);
        assert!(
            !got.pairs
                .iter()
                .any(|p| p.rule_id != "D3" && p.report.ready)
        );
    }

    #[tokio::test]
    async fn shadow_list_pages_newest_first_and_only_shadow_rows() {
        let pool = pool().await;
        let base = NOW_MS - DAY_MS;
        for i in 0..5_i64 {
            shadow_row(&pool, &format!("a{i}"), "s1", base + i * HOUR_MS, |_| {}).await;
        }
        // Same `decided_at` as the newest: `id` desc breaks the tie.
        let tie = shadow_row(&pool, "tie", "s2", base + 4 * HOUR_MS, |_| {}).await;
        // Not part of the list: a live row, another action, another rule.
        shadow_row(&pool, "live", "s3", base, |r| r.mode = "live".into()).await;
        shadow_row(&pool, "prop", "s4", base, |r| {
            r.action = "propose".into();
            r.rule_id = "F1".into();
        })
        .await;
        shadow_row(&pool, "d6", "s5", base, |r| r.rule_id = "D6".into()).await;

        let query = |limit, cursor| ShadowQuery {
            action: Some("inject"),
            rule_id: Some("D3"),
            limit,
            cursor,
        };
        let first = shadow_list(&pool, &query(4, None)).await.unwrap();
        assert_eq!(first.items.len(), 4);
        assert_eq!(first.items[0].id, tie, "ties break by id desc");
        let decided: Vec<_> = first.items.iter().map(|i| i.decided_at.clone()).collect();
        assert!(decided.windows(2).all(|w| w[0] >= w[1]), "{decided:?}");
        let cursor = decode_shadow_cursor(&first.next_cursor.expect("more rows follow")).unwrap();
        let second = shadow_list(&pool, &query(4, Some(cursor))).await.unwrap();
        assert_eq!(second.items.len(), 2);
        assert!(second.next_cursor.is_none());
        let mut ids: Vec<_> = first
            .items
            .iter()
            .chain(&second.items)
            .map(|i| i.id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 6, "no row twice, none lost");

        // Without filters: every shadow row (D3 x6, F1, D6), still no live one.
        let all = shadow_list(
            &pool,
            &ShadowQuery {
                action: None,
                rule_id: None,
                limit: 50,
                cursor: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(all.items.len(), 8);
        assert!(all.items.iter().all(|i| i.id != 0));
    }

    #[tokio::test]
    async fn shadow_list_refuses_an_unknown_action() {
        let pool = pool().await;
        let got = shadow_list(
            &pool,
            &ShadowQuery {
                action: Some("nuke"),
                rule_id: None,
                limit: 10,
                cursor: None,
            },
        )
        .await;
        assert!(matches!(got, Err(ActionError::BadAction)), "{got:?}");
    }

    #[tokio::test]
    async fn a_shadow_item_serializes_the_closed_fields_only() {
        let pool = pool().await;
        shadow_row(&pool, "x", "secret-session", NOW_MS - HOUR_MS, |r| {
            r.decision = "suppress".into();
            r.suppress_reason = Some("cooldown".into());
            r.outcome_state = "pending".into();
            r.outcome = None;
        })
        .await;
        let page = shadow_list(
            &pool,
            &ShadowQuery {
                action: None,
                rule_id: None,
                limit: 10,
                cursor: None,
            },
        )
        .await
        .unwrap();
        let value = serde_json::to_value(&page).unwrap();
        let item = value["items"][0].as_object().unwrap();
        let mut keys: Vec<_> = item.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "action",
                "decided_at",
                "decision",
                "human_verdict",
                "id",
                "outcome",
                "outcome_state",
                "params",
                "project_id",
                "rule_id",
                "suppress_reason",
                "template_id"
            ]
        );
        assert_eq!(item["params"]["count"], 3, "params is an object, not text");
        assert_eq!(item["suppress_reason"], "cooldown");
        assert!(!value.to_string().contains("secret-session"));
    }

    #[test]
    fn the_shadow_cursor_round_trips_and_refuses_garbage() {
        let cursor = ShadowCursor {
            decided_at: "2026-10-05T10:30:00.000Z".into(),
            id: 42,
        };
        let text = encode_shadow_cursor(&cursor);
        assert_eq!(decode_shadow_cursor(&text), Some(cursor));
        assert_eq!(decode_shadow_cursor("not a cursor"), None);
        assert_eq!(decode_shadow_cursor(""), None);
    }

    #[tokio::test]
    async fn mark_shadow_sets_the_verdict_replaces_it_and_is_idempotent() {
        let pool = pool().await;
        let id = shadow_row(&pool, "m", "s1", NOW_MS - HOUR_MS, |_| {}).await;
        for _ in 0..2 {
            let item = mark_shadow(&pool, id, "useful").await.unwrap();
            assert_eq!(item.id, id);
            assert_eq!(item.human_verdict.as_deref(), Some("useful"));
        }
        let item = mark_shadow(&pool, id, "noise").await.unwrap();
        assert_eq!(item.human_verdict.as_deref(), Some("noise"));
        let stored: Option<String> =
            sqlx::query_scalar("SELECT human_verdict FROM devtime_shadow WHERE id = ?")
                .bind(id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(stored.as_deref(), Some("noise"));
    }

    #[tokio::test]
    async fn mark_shadow_refuses_a_bad_verdict_and_rows_that_are_not_shadow_fires() {
        let pool = pool().await;
        let fire = shadow_row(&pool, "f", "s1", NOW_MS - HOUR_MS, |_| {}).await;
        let suppressed = shadow_row(&pool, "s", "s2", NOW_MS - HOUR_MS, |r| {
            r.decision = "suppress".into();
            r.suppress_reason = Some("cooldown".into());
        })
        .await;
        let live = shadow_row(&pool, "l", "s3", NOW_MS - HOUR_MS, |r| {
            r.mode = "live".into()
        })
        .await;
        let bad = mark_shadow(&pool, fire, "maybe").await;
        assert!(matches!(bad, Err(ActionError::BadVerdict)), "{bad:?}");
        for id in [suppressed, live, 9_999] {
            let got = mark_shadow(&pool, id, "useful").await;
            assert!(
                matches!(got, Err(ActionError::UnknownShadow)),
                "{id}: {got:?}"
            );
        }
        let marked: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM devtime_shadow WHERE human_verdict IS NOT NULL",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(marked, 0, "a refused mark writes nothing");
    }

    #[tokio::test]
    async fn promote_pair_refuses_unknown_pairs_and_an_unready_one_without_writing() {
        let pool = pool().await;
        let cfg = DevtimeConfig::default();
        let bad = promote_pair(&pool, &cfg, "nuke", "D3", NOW_MS).await;
        assert!(matches!(bad, Err(ActionError::BadAction)), "{bad:?}");
        for (action, rule) in [("inject", "ZZ"), ("propose", "D3")] {
            let got = promote_pair(&pool, &cfg, action, rule, NOW_MS).await;
            assert!(matches!(got, Err(ActionError::UnknownRule)), "{got:?}");
        }
        let unready = promote_pair(&pool, &cfg, "inject", "D3", NOW_MS).await;
        assert!(matches!(unready, Err(ActionError::NotReady)), "{unready:?}");
        assert_eq!(
            st::get_action_state(&pool, "inject", "D3").await.unwrap(),
            None,
            "nothing is written when the pair is not ready"
        );
    }

    #[tokio::test]
    async fn promote_pair_writes_live_as_the_owner_when_ready() {
        let pool = pool().await;
        seed_ready_d3(&pool).await;
        let pair = promote_pair(&pool, &DevtimeConfig::default(), "inject", "D3", NOW_MS)
            .await
            .unwrap();
        assert_eq!(
            (pair.mode.as_str(), pair.changed_by.as_deref()),
            ("live", Some("owner"))
        );
        assert!(pair.since.is_some());
        let stored = st::get_action_state(&pool, "inject", "D3")
            .await
            .unwrap()
            .expect("the state row was written");
        assert_eq!(
            (stored.mode.as_str(), stored.changed_by.as_str()),
            ("live", "owner")
        );
    }

    #[tokio::test]
    async fn promote_pair_is_blocked_by_a_single_failing_criterion() {
        let pool = pool().await;
        seed_ready_d3(&pool).await;
        // The human marks fall under the minimum of five: only criterion 5 fails.
        sqlx::query("UPDATE devtime_shadow SET human_verdict = NULL WHERE id IN (SELECT id FROM devtime_shadow LIMIT 3)")
            .execute(&pool)
            .await
            .unwrap();
        let got = promote_pair(&pool, &DevtimeConfig::default(), "inject", "D3", NOW_MS).await;
        assert!(matches!(got, Err(ActionError::NotReady)), "{got:?}");
        assert_eq!(
            st::get_action_state(&pool, "inject", "D3").await.unwrap(),
            None
        );
    }
}
