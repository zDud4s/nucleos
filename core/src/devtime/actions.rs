//! The automatic actions' decision step (actions design spec, section 3). `decide_*` are pure
//! functions: project state and config in, one [`Decision`] out, ready to be written to
//! `devtime_shadow`. No message text is ever carried here: only ids, counts and milliseconds.

#![allow(dead_code)] // the wiring step (2.4) is the caller

pub mod inject;
pub mod outcome;
pub mod propose;

use crate::devtime::precision::{RulePrecision, action_eligible};

/// One decision, shaped like a `devtime_shadow` row before it is stored. Its string fields come from
/// the closed vocabularies in `devtime::store::shadow` (`ACTIONS`, `MODES`, `DECISIONS`,
/// `SUPPRESS_REASONS`).
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    /// `inject` or `propose`.
    pub action: String,
    pub rule_id: String,
    pub project_id: String,
    /// The session the decision is for (`inject`); `None` for a cross-session `propose`.
    pub session_id: Option<String>,
    /// `inject`: `rule_id|project_id`; `propose`: the cross finding's key.
    pub subject_key: String,
    /// `shadow` or `live`.
    pub mode: String,
    /// `fire` or `suppress`.
    pub decision: String,
    /// Set exactly when `decision` is `suppress`.
    pub suppress_reason: Option<String>,
    pub basis_precision: Option<f64>,
    pub basis_cases: Option<i64>,
    pub basis_recent_count: Option<i64>,
    pub basis_recent_cost_ms: Option<i64>,
    /// A-priori estimate: `0.5 x median(cost_ms)` over the rule's findings in the window.
    pub expected_saving_ms: Option<i64>,
    pub rule_version: i64,
    pub action_version: i64,
}

/// One finding of a rule in the project, inside the look-back window: only its cost matters here.
#[derive(Debug, Clone, PartialEq)]
pub struct RecentFinding {
    pub rule_id: String,
    pub cost_ms: i64,
}

/// What `decide_*` reads about one project when a session starts.
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectState {
    pub project_id: String,
    /// The session that is starting.
    pub session_id: Option<String>,
    /// `shadow` or `live`, from `devtime_action_state`.
    pub mode: String,
    /// The project's findings in the last `inject_window_days`, all rules.
    pub recent: Vec<RecentFinding>,
    /// Precision of every rule, as `precision::rule_report` returns it.
    pub precisions: Vec<RulePrecision>,
}

/// PURE: decides `inject` for one rule (D3 or D6) in one project.
///
/// `fire` when the rule has at least `inject_min_count` findings costing at least
/// `inject_min_cost_ms` in total and `action_eligible` holds; `suppress/not_eligible` when the rule
/// is not eligible; `suppress/below_threshold` otherwise.
pub fn decide_inject(
    state: &ProjectState,
    rule_id: &str,
    cfg: &crate::config::DevtimeActionsConfig,
    action_version: i64,
) -> Decision {
    let precision = state.precisions.iter().find(|p| p.rule_id == rule_id);
    let mut costs: Vec<i64> = state
        .recent
        .iter()
        .filter(|f| f.rule_id == rule_id)
        .map(|f| f.cost_ms)
        .collect();
    let count = i64::try_from(costs.len()).unwrap_or(i64::MAX);
    let total = costs.iter().fold(0_i64, |acc, c| acc.saturating_add(*c));
    let eligible = precision.is_some_and(|p| action_eligible(p, cfg));
    let suppress_reason = if !eligible {
        Some("not_eligible")
    } else if count < i64::from(cfg.inject_min_count)
        || total < i64::try_from(cfg.inject_min_cost_ms).unwrap_or(i64::MAX)
    {
        Some("below_threshold")
    } else {
        None
    };
    let decision = if suppress_reason.is_some() {
        "suppress"
    } else {
        "fire"
    };
    Decision {
        action: "inject".to_string(),
        rule_id: rule_id.to_string(),
        project_id: state.project_id.clone(),
        session_id: state.session_id.clone(),
        subject_key: format!("{rule_id}|{}", state.project_id),
        mode: state.mode.clone(),
        decision: decision.to_string(),
        suppress_reason: suppress_reason.map(str::to_string),
        basis_precision: precision.and_then(|p| p.precision),
        basis_cases: precision.map(|p| p.n_cases),
        basis_recent_count: Some(count),
        basis_recent_cost_ms: Some(total),
        expected_saving_ms: median(&mut costs).map(|m| m / 2),
        rule_version: crate::devtime::rules::rule(rule_id).map_or(1, |r| i64::from(r.version)),
        action_version,
    }
}

/// The version of the action logic; a bump starts a fresh cooldown history.
pub const ACTION_VERSION: i64 = 1;

/// One earlier `fire` of `inject`, read back from `devtime_shadow` for the cooldown.
#[derive(Debug, Clone, PartialEq)]
pub struct PriorFire {
    pub rule_id: String,
    pub project_id: String,
    /// `shadow` or `live`: a fire only counts toward the cooldown of its own mode.
    pub mode: String,
    pub action_version: i64,
    /// When the fire was decided, in unix milliseconds.
    pub at_ms: i64,
}

/// PURE: applies the per-session cap and the per-(rule, project) cooldown to the `inject`
/// candidates of one session.
///
/// Cooldown first: a `fire` whose (rule, project, mode, action_version) has a `PriorFire` within
/// `inject_cooldown_days` of `now_ms` becomes `suppress/cooldown`. Then, among the surviving
/// `fire`s, the top `inject_max_per_session` by `basis_recent_cost_ms` desc (ties by `rule_id`
/// asc) stay; the rest become `suppress/cap`. Decisions already `suppress` pass untouched and take
/// no cap slot. The output is sorted by (`rule_id`, `project_id`), whatever the input order.
pub fn apply_cap_and_cooldown(
    decisions: Vec<Decision>,
    prior: &[PriorFire],
    now_ms: i64,
    cfg: &crate::config::DevtimeActionsConfig,
) -> Vec<Decision> {
    let window_ms = i64::from(cfg.inject_cooldown_days).saturating_mul(86_400_000);
    let mut out = decisions;
    // Cooldown first, so a cooled-down fire takes no cap slot.
    for d in out.iter_mut().filter(|d| d.decision == "fire") {
        let cooled = prior.iter().any(|p| {
            p.rule_id == d.rule_id
                && p.project_id == d.project_id
                && p.mode == d.mode
                && p.action_version == d.action_version
                && now_ms.saturating_sub(p.at_ms) < window_ms
        });
        if cooled {
            d.decision = "suppress".to_string();
            d.suppress_reason = Some("cooldown".to_string());
        }
    }
    // Cap: rank the surviving fires by cost desc, ties by rule_id asc (then project_id).
    let mut fires: Vec<usize> = (0..out.len())
        .filter(|&i| out[i].decision == "fire")
        .collect();
    fires.sort_by(|&a, &b| {
        out[b]
            .basis_recent_cost_ms
            .unwrap_or(0)
            .cmp(&out[a].basis_recent_cost_ms.unwrap_or(0))
            .then_with(|| out[a].rule_id.cmp(&out[b].rule_id))
            .then_with(|| out[a].project_id.cmp(&out[b].project_id))
    });
    let keep = usize::try_from(cfg.inject_max_per_session).unwrap_or(usize::MAX);
    for &i in fires.iter().skip(keep) {
        out[i].decision = "suppress".to_string();
        out[i].suppress_reason = Some("cap".to_string());
    }
    out.sort_by(|a, b| {
        a.rule_id
            .cmp(&b.rule_id)
            .then_with(|| a.project_id.cmp(&b.project_id))
    });
    out
}

/// Wiring step (2.4): decides and records the automatic actions for one session the rules pass
/// just ran for. Records, never delivers. Returns the number of `devtime_shadow` rows newly written.
///
/// `now_ms` is unix milliseconds and becomes `decided_at`. Contract in the wiring tests below.
pub async fn run_for_session(
    pool: &sqlx::SqlitePool,
    cfg: &crate::config::DevtimeConfig,
    session_id: &str,
    now_ms: i64,
) -> sqlx::Result<usize> {
    use crate::devtime::store as st;
    let acfg = &cfg.actions;
    if !acfg.enabled {
        return Ok(0);
    }
    let head: Option<(String, Option<String>, String)> = sqlx::query_as(
        "SELECT project_id, ended_at, started_at FROM devtime_sessions WHERE session_id = ?",
    )
    .bind(session_id)
    .fetch_optional(pool)
    .await?;
    let Some((project_id, ended_at, started_at)) = head else {
        return Ok(0);
    };
    let last_ms = ended_at
        .as_deref()
        .and_then(parse_ms)
        .or_else(|| parse_ms(&started_at))
        .unwrap_or(now_ms);
    let decided_at = ms_text(now_ms);
    let precisions = crate::devtime::precision::rule_report(pool, &cfg.rules, None).await?;
    let mut written = 0_usize;

    // ---- inject: one candidate per inject-pair rule with a finding in this session ----
    let session_findings: Vec<st::FindingRow> = sqlx::query_as(
        "SELECT * FROM devtime_findings WHERE session_id = ? AND scope <> 'cross' ORDER BY finding_key",
    )
    .bind(session_id)
    .fetch_all(pool)
    .await?;
    let since = ms_text(
        now_ms.saturating_sub(i64::from(acfg.inject_window_days).saturating_mul(86_400_000)),
    );
    let recent: Vec<RecentFinding> = sqlx::query_as::<_, (String, i64)>(
        "SELECT rule_id, cost_ms FROM devtime_findings WHERE project_id = ? AND scope <> 'cross' \
         AND started_at >= ?",
    )
    .bind(&project_id)
    .bind(&since)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|(rule_id, cost_ms)| RecentFinding { rule_id, cost_ms })
    .collect();
    let inject_window_end =
        ms_text(last_ms.saturating_add(i64::try_from(cfg.idle_minutes).unwrap_or(0) * 60_000));
    let mut candidates: Vec<(Decision, String)> = Vec::new();
    let mut prior: Vec<PriorFire> = Vec::new();
    for rule_id in acfg.pairs.get("inject").map(Vec::as_slice).unwrap_or(&[]) {
        let Some(finding) = session_findings.iter().find(|f| &f.rule_id == rule_id) else {
            continue;
        };
        let Some(mode) = pair_mode(pool, "inject", rule_id).await? else {
            continue;
        };
        let state = ProjectState {
            project_id: project_id.clone(),
            session_id: Some(session_id.to_string()),
            mode: mode.clone(),
            recent: recent.clone(),
            precisions: precisions.clone(),
        };
        candidates.push((
            decide_inject(&state, rule_id, acfg, ACTION_VERSION),
            finding.rules_version.clone(),
        ));
        for row in st::decisions_for(pool, "inject", rule_id, &mode, &[ACTION_VERSION]).await? {
            // This session's own earlier row must not cool down its recompute.
            if row.decision == "fire" && row.session_id.as_deref() != Some(session_id) {
                prior.push(PriorFire {
                    rule_id: row.rule_id,
                    project_id: row.project_id,
                    mode: row.mode,
                    action_version: row.action_version,
                    at_ms: parse_ms(&row.decided_at).unwrap_or(0),
                });
            }
        }
    }
    let decided = apply_cap_and_cooldown(
        candidates.iter().map(|(d, _)| d.clone()).collect(),
        &prior,
        now_ms,
        acfg,
    );
    for d in decided {
        let rules_version = candidates
            .iter()
            .find(|(c, _)| c.rule_id == d.rule_id)
            .map(|(_, v)| v.as_str())
            .unwrap_or_default();
        if write_decision(
            pool,
            acfg,
            &d,
            rules_version,
            &decided_at,
            &inject_window_end,
        )
        .await?
        {
            written += 1;
        }
    }

    // ---- propose: cross findings of propose-pair rules whose sessions include this one ----
    let cross: Vec<st::FindingRow> = sqlx::query_as(
        "SELECT * FROM devtime_findings WHERE project_id = ? AND scope = 'cross' ORDER BY finding_key",
    )
    .bind(&project_id)
    .fetch_all(pool)
    .await?;
    let mut open: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM devtime_shadow WHERE action = 'propose' AND project_id = ? \
         AND decision = 'fire' AND outcome_state = 'pending'",
    )
    .bind(&project_id)
    .fetch_one(pool)
    .await?;
    let mut already: Vec<String> = sqlx::query_scalar(
        "SELECT subject_key FROM devtime_shadow WHERE action = 'propose' AND project_id = ?",
    )
    .bind(&project_id)
    .fetch_all(pool)
    .await?;
    let propose_window_end = ms_text(
        last_ms.saturating_add(i64::from(acfg.propose_window_days).saturating_mul(86_400_000)),
    );
    for rule_id in acfg.pairs.get("propose").map(Vec::as_slice).unwrap_or(&[]) {
        let Some(mode) = pair_mode(pool, "propose", rule_id).await? else {
            continue;
        };
        for f in cross.iter().filter(|f| &f.rule_id == rule_id) {
            let sessions: Vec<String> = serde_json::from_str(&f.sessions).unwrap_or_default();
            if !sessions.iter().any(|s| s == session_id) {
                continue;
            }
            let state = propose::ProposeState {
                project_id: project_id.clone(),
                mode: mode.clone(),
                candidates: Vec::new(),
                open_proposals: open,
                already_proposed: already.clone(),
                precisions: precisions.clone(),
            };
            let candidate = propose::CrossCandidate {
                finding_key: f.finding_key.clone(),
                rule_id: f.rule_id.clone(),
                sessions: i64::try_from(sessions.len()).unwrap_or(i64::MAX),
                cost_ms: f.cost_ms,
                count: f.count,
            };
            let d = propose::decide_propose(&state, &candidate, acfg, ACTION_VERSION);
            if write_decision(
                pool,
                acfg,
                &d,
                &f.rules_version,
                &decided_at,
                &propose_window_end,
            )
            .await?
            {
                written += 1;
            }
            if d.decision == "fire" {
                open += 1;
            }
            already.push(f.finding_key.clone());
        }
    }
    Ok(written)
}

/// The mode a pair decides under: `None` when the owner turned it `off`; no row means `shadow`.
async fn pair_mode(
    pool: &sqlx::SqlitePool,
    action: &str,
    rule_id: &str,
) -> sqlx::Result<Option<String>> {
    let state = crate::devtime::store::get_action_state(pool, action, rule_id).await?;
    Ok(match state {
        Some(row) if row.mode == "off" => None,
        Some(row) => Some(row.mode),
        None => Some("shadow".to_string()),
    })
}

/// Stores one decision as a `pending` row, suppressed ones included. True when a row was written.
async fn write_decision(
    pool: &sqlx::SqlitePool,
    cfg: &crate::config::DevtimeActionsConfig,
    d: &Decision,
    rules_version: &str,
    decided_at: &str,
    window_end_at: &str,
) -> sqlx::Result<bool> {
    use crate::devtime::store as st;
    let template_id = match (d.action.as_str(), d.rule_id.as_str()) {
        ("inject", "D3") => "inject.d3.v1",
        ("inject", "D6") => "inject.d6.v1",
        ("propose", "F1") => "propose.f1.v1",
        ("propose", "F2") => "propose.f2.v1",
        // No template in the closed catalogue: nothing to record.
        _ => return Ok(false),
    };
    st::insert_decision(
        pool,
        &st::ShadowRow {
            action: d.action.clone(),
            rule_id: d.rule_id.clone(),
            project_id: d.project_id.clone(),
            session_id: d.session_id.clone(),
            subject_key: d.subject_key.clone(),
            mode: d.mode.clone(),
            decision: d.decision.clone(),
            suppress_reason: d.suppress_reason.clone(),
            template_id: template_id.to_string(),
            basis_precision: d.basis_precision,
            basis_cases: d.basis_cases,
            basis_recent_count: d.basis_recent_count,
            basis_recent_cost_ms: d.basis_recent_cost_ms,
            expected_saving_ms: d.expected_saving_ms,
            rule_version: d.rule_version,
            action_version: d.action_version,
            thresholds_hash: st::thresholds_hash(cfg, &d.action, &d.rule_id),
            rules_version: rules_version.to_string(),
            decided_at: decided_at.to_string(),
            window_end_at: window_end_at.to_string(),
            outcome_state: "pending".to_string(),
            ..Default::default()
        },
    )
    .await
}

/// Unix milliseconds to the `%Y-%m-%dT%H:%M:%S%.3fZ` text the devtime tables store.
fn ms_text(ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ms)
        .unwrap_or_default()
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

fn parse_ms(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|t| t.timestamp_millis())
}

/// Median of `values` (sorted in place); the mean of the two middle ones for an even count.
fn median(values: &mut [i64]) -> Option<i64> {
    values.sort_unstable();
    let n = values.len();
    if n == 0 {
        return None;
    }
    let mid = n / 2;
    if n % 2 == 1 {
        Some(values[mid])
    } else {
        Some(values[mid - 1].saturating_add(values[mid]) / 2)
    }
}

#[cfg(test)]
mod wiring {
    use super::*;
    use crate::config::DevtimeConfig;
    use sqlx::SqlitePool;

    const SID: &str = "s-wire";
    const LAST: &str = "2026-10-05T10:30:00.000Z";
    /// `LAST` in unix ms.
    const LAST_MS: i64 = 1_791_196_200_000;
    const NOW_MS: i64 = LAST_MS + 60_000;

    async fn pool() -> SqlitePool {
        let pool = crate::testdb::fresh_pool().await;
        sqlx::query(
            "INSERT INTO devtime_sessions (session_id, project_id, started_at, ended_at, dirty, \
             parser_version, updated_at) VALUES (?, 'p1', '2026-10-05T10:00:00.000Z', ?, 0, 1, ?)",
        )
        .bind(SID)
        .bind(LAST)
        .bind(LAST)
        .execute(&pool)
        .await
        .unwrap();
        pool
    }

    /// One finding row; `scope` is `session` or `cross`.
    async fn finding(pool: &SqlitePool, key: &str, rule: &str, scope: &str, cost_ms: i64) {
        sqlx::query(
            "INSERT INTO devtime_findings (finding_key, project_id, session_id, scope, rule_id, \
             rule_version, level, waste, lever, confidence, lane, started_at, ended_at, cost_ms, \
             count, sessions, rules_version, parser_version) VALUES (?, 'p1', ?, ?, ?, 1, 'session', \
             'rework', 'x', 'exact', 'main', ?, ?, ?, 4, ?, 'rv', 1)",
        )
        .bind(key)
        .bind(SID)
        .bind(scope)
        .bind(rule)
        .bind(LAST)
        .bind(LAST)
        .bind(cost_ms)
        .bind(format!("[\"{SID}\",\"s2\",\"s3\"]"))
        .execute(pool)
        .await
        .unwrap();
    }

    async fn seed(pool: &SqlitePool) {
        for i in 0..4 {
            finding(pool, &format!("D3-{i}"), "D3", "session", 60_000).await;
        }
        finding(pool, "F1-cross", "F1", "cross", 900_000).await;
    }

    async fn rows(pool: &SqlitePool, action: &str) -> Vec<(String, String, String)> {
        sqlx::query_as(
            "SELECT rule_id, outcome_state, window_end_at FROM devtime_shadow WHERE action = ? \
             ORDER BY rule_id, id",
        )
        .bind(action)
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn total(pool: &SqlitePool) -> i64 {
        sqlx::query_scalar("SELECT COUNT(*) FROM devtime_shadow")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn ms_text(ms: i64) -> String {
        chrono::DateTime::from_timestamp_millis(ms)
            .unwrap()
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string()
    }

    // Contract: one `inject` row per inject-pair rule (cfg.actions.pairs) that has at least one
    // finding in the session, and one `propose` row per `cross` finding of a propose-pair rule whose
    // `sessions` JSON lists the session. Suppressed decisions are recorded too. All rows `pending`.

    #[tokio::test]
    async fn wiring_session_with_findings_writes_pending_rows() {
        let pool = pool().await;
        seed(&pool).await;
        let n = run_for_session(&pool, &DevtimeConfig::default(), SID, NOW_MS)
            .await
            .unwrap();
        assert_eq!(n, 2);
        let inject = rows(&pool, "inject").await;
        let propose = rows(&pool, "propose").await;
        assert_eq!(inject.len(), 1);
        assert_eq!(inject[0].0, "D3");
        assert_eq!(propose.len(), 1);
        assert_eq!(propose[0].0, "F1");
        assert!(inject.iter().chain(&propose).all(|r| r.1 == "pending"));
    }

    #[tokio::test]
    async fn wiring_recompute_twice_does_not_duplicate() {
        let pool = pool().await;
        seed(&pool).await;
        let cfg = DevtimeConfig::default();
        run_for_session(&pool, &cfg, SID, NOW_MS).await.unwrap();
        let first = total(&pool).await;
        assert!(first > 0);
        let again = run_for_session(&pool, &cfg, SID, NOW_MS + 1_000)
            .await
            .unwrap();
        assert_eq!(again, 0);
        assert_eq!(total(&pool).await, first);
    }

    #[tokio::test]
    async fn wiring_disabled_writes_nothing() {
        let pool = pool().await;
        seed(&pool).await;
        let mut cfg = DevtimeConfig::default();
        cfg.actions.enabled = false;
        let n = run_for_session(&pool, &cfg, SID, NOW_MS).await.unwrap();
        assert_eq!(n, 0);
        assert_eq!(total(&pool).await, 0);
    }

    #[tokio::test]
    async fn wiring_mode_off_writes_nothing() {
        let pool = pool().await;
        seed(&pool).await;
        for (action, rule) in [("inject", "D3"), ("propose", "F1")] {
            crate::devtime::store::set_action_state(
                &pool,
                &crate::devtime::store::ActionStateRow {
                    action: action.to_string(),
                    rule_id: rule.to_string(),
                    mode: "off".to_string(),
                    since: String::new(),
                    changed_by: "owner".to_string(),
                    reason: None,
                },
            )
            .await
            .unwrap();
        }
        let n = run_for_session(&pool, &DevtimeConfig::default(), SID, NOW_MS)
            .await
            .unwrap();
        assert_eq!(n, 0);
        assert_eq!(total(&pool).await, 0);
    }

    #[tokio::test]
    async fn wiring_window_end_inject_is_last_activity_plus_idle() {
        let pool = pool().await;
        seed(&pool).await;
        let cfg = DevtimeConfig::default();
        run_for_session(&pool, &cfg, SID, NOW_MS).await.unwrap();
        let idle_ms = i64::try_from(cfg.idle_minutes).unwrap() * 60_000;
        let inject = rows(&pool, "inject").await;
        assert_eq!(inject.len(), 1);
        assert_eq!(inject[0].2, ms_text(LAST_MS + idle_ms));
    }

    #[tokio::test]
    async fn wiring_window_end_propose_is_plus_propose_window_days() {
        let pool = pool().await;
        seed(&pool).await;
        let cfg = DevtimeConfig::default();
        run_for_session(&pool, &cfg, SID, NOW_MS).await.unwrap();
        let window_ms = i64::from(cfg.actions.propose_window_days) * 86_400_000;
        let propose = rows(&pool, "propose").await;
        assert_eq!(propose.len(), 1);
        assert_eq!(propose[0].2, ms_text(LAST_MS + window_ms));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY_MS: i64 = 86_400_000;
    const NOW: i64 = 100 * DAY_MS;

    fn cfg() -> crate::config::DevtimeActionsConfig {
        crate::config::DevtimeActionsConfig::default()
    }

    fn cand(rule: &str, cost: i64, mode: &str) -> Decision {
        Decision {
            action: "inject".to_string(),
            rule_id: rule.to_string(),
            project_id: "p1".to_string(),
            session_id: Some("s1".to_string()),
            subject_key: format!("{rule}|p1"),
            mode: mode.to_string(),
            decision: "fire".to_string(),
            suppress_reason: None,
            basis_precision: Some(0.9),
            basis_cases: Some(20),
            basis_recent_count: Some(5),
            basis_recent_cost_ms: Some(cost),
            expected_saving_ms: Some(cost / 10),
            rule_version: 1,
            action_version: ACTION_VERSION,
        }
    }

    fn prior(rule: &str, mode: &str, version: i64, days_ago: i64) -> PriorFire {
        PriorFire {
            rule_id: rule.to_string(),
            project_id: "p1".to_string(),
            mode: mode.to_string(),
            action_version: version,
            at_ms: NOW - days_ago * DAY_MS,
        }
    }

    fn find<'a>(out: &'a [Decision], rule: &str) -> &'a Decision {
        out.iter()
            .find(|d| d.rule_id == rule)
            .expect("rule in output")
    }

    #[test]
    fn cap_cooldown_cap_keeps_top_by_cost_and_suppresses_rest() {
        let input = vec![
            cand("D1", 100, "live"),
            cand("D2", 500, "live"),
            cand("D3", 300, "live"),
            cand("D4", 400, "live"),
            cand("D5", 200, "live"),
        ];
        let out = apply_cap_and_cooldown(input, &[], NOW, &cfg());
        assert_eq!(out.len(), 5);
        for kept in ["D2", "D4", "D3"] {
            let d = find(&out, kept);
            assert_eq!(d.decision, "fire", "{kept}");
            assert_eq!(d.suppress_reason, None);
        }
        for dropped in ["D1", "D5"] {
            let d = find(&out, dropped);
            assert_eq!(d.decision, "suppress", "{dropped}");
            assert_eq!(d.suppress_reason.as_deref(), Some("cap"));
        }
    }

    #[test]
    fn cap_cooldown_prior_fire_inside_window_suppresses() {
        let input = vec![cand("D3", 300, "live")];
        let out = apply_cap_and_cooldown(
            input,
            &[prior("D3", "live", ACTION_VERSION, 1)],
            NOW,
            &cfg(),
        );
        assert_eq!(out[0].decision, "suppress");
        assert_eq!(out[0].suppress_reason.as_deref(), Some("cooldown"));
    }

    #[test]
    fn cap_cooldown_prior_fire_outside_window_does_not_suppress() {
        let input = vec![cand("D3", 300, "live")];
        let days = i64::from(cfg().inject_cooldown_days) + 1;
        let out = apply_cap_and_cooldown(
            input,
            &[prior("D3", "live", ACTION_VERSION, days)],
            NOW,
            &cfg(),
        );
        assert_eq!(out[0].decision, "fire");
        assert_eq!(out[0].suppress_reason, None);
    }

    #[test]
    fn cap_cooldown_applies_in_shadow_like_live() {
        let input = vec![cand("D3", 300, "shadow")];
        let out = apply_cap_and_cooldown(
            input,
            &[prior("D3", "shadow", ACTION_VERSION, 1)],
            NOW,
            &cfg(),
        );
        assert_eq!(out[0].decision, "suppress");
        assert_eq!(out[0].suppress_reason.as_deref(), Some("cooldown"));
    }

    #[test]
    fn cap_cooldown_fire_of_other_mode_does_not_count() {
        let input = vec![cand("D3", 300, "live")];
        let out = apply_cap_and_cooldown(
            input,
            &[prior("D3", "shadow", ACTION_VERSION, 1)],
            NOW,
            &cfg(),
        );
        assert_eq!(out[0].decision, "fire");
    }

    #[test]
    fn cap_cooldown_other_rule_or_project_does_not_count() {
        let mut other_project = prior("D3", "live", ACTION_VERSION, 1);
        other_project.project_id = "p2".to_string();
        let priors = [other_project, prior("D6", "live", ACTION_VERSION, 1)];
        let out = apply_cap_and_cooldown(vec![cand("D3", 300, "live")], &priors, NOW, &cfg());
        assert_eq!(out[0].decision, "fire");
    }

    #[test]
    fn cap_cooldown_new_action_version_does_not_inherit_old_cooldown() {
        let mut c = cand("D3", 300, "live");
        c.action_version = ACTION_VERSION + 1;
        let out = apply_cap_and_cooldown(
            vec![c],
            &[prior("D3", "live", ACTION_VERSION, 1)],
            NOW,
            &cfg(),
        );
        assert_eq!(out[0].decision, "fire");
    }

    #[test]
    fn cap_cooldown_cooled_down_fire_takes_no_cap_slot() {
        let input = vec![
            cand("D1", 900, "live"),
            cand("D2", 500, "live"),
            cand("D3", 400, "live"),
            cand("D4", 300, "live"),
        ];
        let out = apply_cap_and_cooldown(
            input,
            &[prior("D1", "live", ACTION_VERSION, 1)],
            NOW,
            &cfg(),
        );
        assert_eq!(
            find(&out, "D1").suppress_reason.as_deref(),
            Some("cooldown")
        );
        for kept in ["D2", "D3", "D4"] {
            assert_eq!(find(&out, kept).decision, "fire", "{kept}");
        }
    }

    #[test]
    fn cap_cooldown_is_deterministic_with_ties_and_any_input_order() {
        let a = vec![
            cand("D1", 200, "live"),
            cand("D2", 200, "live"),
            cand("D3", 200, "live"),
            cand("D4", 200, "live"),
            cand("D5", 900, "live"),
        ];
        let mut b = a.clone();
        b.reverse();
        let mut c = a.clone();
        c.rotate_left(2);
        let out_a = apply_cap_and_cooldown(a, &[], NOW, &cfg());
        assert_eq!(out_a, apply_cap_and_cooldown(b, &[], NOW, &cfg()));
        assert_eq!(out_a, apply_cap_and_cooldown(c, &[], NOW, &cfg()));
        // Tie on cost breaks by rule_id asc: D5 (highest), then D1, D2 stay; D3, D4 are capped.
        assert_eq!(find(&out_a, "D5").decision, "fire");
        assert_eq!(find(&out_a, "D1").decision, "fire");
        assert_eq!(find(&out_a, "D2").decision, "fire");
        assert_eq!(find(&out_a, "D3").suppress_reason.as_deref(), Some("cap"));
        assert_eq!(find(&out_a, "D4").suppress_reason.as_deref(), Some("cap"));
    }

    #[test]
    fn cap_cooldown_already_suppressed_pass_intact_and_take_no_slot() {
        let mut below = cand("D0", 9_999, "live");
        below.decision = "suppress".to_string();
        below.suppress_reason = Some("below_threshold".to_string());
        let input = vec![
            below.clone(),
            cand("D1", 300, "live"),
            cand("D2", 200, "live"),
            cand("D3", 100, "live"),
        ];
        let out = apply_cap_and_cooldown(input, &[], NOW, &cfg());
        assert_eq!(find(&out, "D0"), &below);
        for kept in ["D1", "D2", "D3"] {
            assert_eq!(find(&out, kept).decision, "fire", "{kept}");
        }
    }
}
