//! Metric 10 of the devtime overview (spec 2026-10-17-analytics-devtime §3 row 10, §6.1 and §8):
//! the current window's agent attempts folded per agent type into outcomes, the rule engine's
//! verification verdict, time and role.
//!
//! No SQL: a pure fold over `store::agent_rows`. The outcome fold mirrors the rule engine's
//! `failed_agent` (`devtime/rules/a.rs`): a foreground `error`, or a background task whose
//! notification said `failed` or `killed`, is a failure whatever else the row says. Verification is
//! the per-attempt verdict the rules pass already wrote as the attempt mark; `unruled` is an attempt
//! whose session has had no rules pass yet, so no verdict exists to report.
//!
//! The role comes from the rules globs (`[devtime.rules.roles]`) at request time, not from the
//! store: the owner can edit the globs, and a stored role would keep the old answer. No row carries
//! a session, attempt or agent id (§8): the panel shows which kind of agent works, never which run.

use std::collections::BTreeMap;

use serde::Serialize;
use sqlx::SqlitePool;

use super::{LIST_CAP, OTHER_KEY, UNKNOWN, ratio};
use crate::config::{DevtimeRolesConfig, DevtimeRulesConfig};
use crate::devtime::rules::Role;
use crate::devtime::rules::cmd::role_of;
use crate::devtime::store::{self as store, AgentAttemptRow};
use crate::devtime::unexplained::median_of_sorted;

/// How agent attempts ended; the four counts sum to the attempts.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AgentOutcomes {
    /// A foreground `ok`, or a background launch whose task completed.
    pub ok: i64,
    /// A foreground `error`, or a background task that failed or was killed.
    pub failed: i64,
    /// The owner stopped it.
    pub interrupted: i64,
    /// Still running in the background, no result seen yet, or an unknown token.
    pub other: i64,
}

/// The rule engine's verdicts on agent attempts; the four counts sum to the attempts.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AgentVerification {
    pub passed: i64,
    pub failed: i64,
    /// Marked, but neither passed nor failed (any other mark token folds here).
    pub not_measured: i64,
    /// No mark: the session has had no rules pass yet.
    pub unruled: i64,
}

/// One agent type's attempts in the window.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct AgentTypeRow {
    /// The `agentType`, `unknown` when the call named none, `__other__` past the cap.
    pub agent_type: String,
    /// `implementer` / `reviewer` from the rules globs; `None` when no glob matches.
    pub role: Option<&'static str>,
    pub attempts: i64,
    /// Summed over the attempts that ended.
    pub ms: i64,
    /// Over the attempts that ended; `None` when none has.
    pub median_ms: Option<i64>,
    pub outcome: AgentOutcomes,
    pub verified: AgentVerification,
    /// `outcome.failed / attempts`.
    pub failure_rate: Option<f64>,
    /// `verified.passed / (passed + failed)`; `None` when no attempt was ruled either way.
    pub pass_rate: Option<f64>,
}

/// Metric 10: every agent attempt of the window, and the same per agent type.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Agents {
    pub attempts: i64,
    pub ms: i64,
    pub outcome: AgentOutcomes,
    pub verified: AgentVerification,
    pub failure_rate: Option<f64>,
    pub pass_rate: Option<f64>,
    /// Most attempts first, then by name; past the cap the rest fold into one `__other__` row.
    pub by_type: Vec<AgentTypeRow>,
}

/// What metric 10 read: the current window's agent attempts and the globs that name their roles.
#[derive(Debug, Default)]
pub(crate) struct AgentReads {
    rows: Vec<AgentAttemptRow>,
    roles: DevtimeRolesConfig,
}

pub(crate) async fn read(
    pool: &SqlitePool,
    rules: &DevtimeRulesConfig,
    project: Option<&str>,
    since: &str,
    until: &str,
) -> sqlx::Result<AgentReads> {
    Ok(prepare(
        store::agent_rows(pool, project, since, until).await?,
        rules,
    ))
}

/// PURE: keeps the rows and the roles of the rules in force for this request.
pub(crate) fn prepare(rows: Vec<AgentAttemptRow>, rules: &DevtimeRulesConfig) -> AgentReads {
    AgentReads {
        rows,
        roles: rules.roles.clone(),
    }
}

/// The counters of one group of attempts; `ms` holds the ended attempts' durations, clamped at 0.
#[derive(Default)]
struct Tally {
    attempts: i64,
    outcome: AgentOutcomes,
    verified: AgentVerification,
    ms: Vec<i64>,
}

impl Tally {
    fn add(&mut self, row: &AgentAttemptRow) {
        self.attempts += 1;
        let background = row.bg_status.as_deref();
        let completed = row.outcome == "launched" && background == Some("completed");
        let outcome = &mut self.outcome;
        if row.outcome == "error" || matches!(background, Some("failed" | "killed")) {
            outcome.failed += 1;
        } else if row.outcome == "interrupted" {
            outcome.interrupted += 1;
        } else if row.outcome == "ok" || completed {
            outcome.ok += 1;
        } else {
            outcome.other += 1;
        }
        let verified = &mut self.verified;
        match row.verified.as_deref() {
            Some("passed") => verified.passed += 1,
            Some("failed") => verified.failed += 1,
            Some(_) => verified.not_measured += 1,
            None => verified.unruled += 1,
        }
        if let Some(ms) = row.ms {
            self.ms.push(ms.max(0));
        }
    }

    fn merge(&mut self, other: Tally) {
        self.attempts += other.attempts;
        self.outcome.ok += other.outcome.ok;
        self.outcome.failed += other.outcome.failed;
        self.outcome.interrupted += other.outcome.interrupted;
        self.outcome.other += other.outcome.other;
        self.verified.passed += other.verified.passed;
        self.verified.failed += other.verified.failed;
        self.verified.not_measured += other.verified.not_measured;
        self.verified.unruled += other.verified.unruled;
        self.ms.extend(other.ms);
    }

    fn ms_total(&self) -> i64 {
        self.ms.iter().sum()
    }

    fn failure_rate(&self) -> Option<f64> {
        ratio(self.outcome.failed, self.attempts)
    }

    fn pass_rate(&self) -> Option<f64> {
        ratio(
            self.verified.passed,
            self.verified.passed + self.verified.failed,
        )
    }
}

fn role_name(role: Role) -> &'static str {
    match role {
        Role::Implementer => "implementer",
        Role::Reviewer => "reviewer",
    }
}

fn type_row(agent_type: String, mut tally: Tally, roles: &DevtimeRolesConfig) -> AgentTypeRow {
    tally.ms.sort_unstable();
    let median_ms = (!tally.ms.is_empty()).then(|| median_of_sorted(&tally.ms));
    let role = if agent_type == UNKNOWN || agent_type == OTHER_KEY {
        None
    } else {
        role_of(&agent_type, roles).map(role_name)
    };
    AgentTypeRow {
        role,
        attempts: tally.attempts,
        ms: tally.ms_total(),
        median_ms,
        failure_rate: tally.failure_rate(),
        pass_rate: tally.pass_rate(),
        outcome: tally.outcome,
        verified: tally.verified,
        agent_type,
    }
}

/// PURE: the window's agent attempts folded into metric 10's block.
pub(crate) fn block(reads: &AgentReads) -> Agents {
    let mut total = Tally::default();
    let mut by_type: BTreeMap<String, Tally> = BTreeMap::new();
    for row in &reads.rows {
        total.add(row);
        let key = row.agent_type.as_deref().unwrap_or(UNKNOWN).to_owned();
        by_type.entry(key).or_default().add(row);
    }
    // The map is in name order and the sort is stable, so ties stay by name.
    let mut types: Vec<(String, Tally)> = by_type.into_iter().collect();
    types.sort_by_key(|(_, t)| std::cmp::Reverse(t.attempts));
    let rest = if types.len() > LIST_CAP {
        types.split_off(LIST_CAP)
    } else {
        Vec::new()
    };
    let mut rows: Vec<AgentTypeRow> = types
        .into_iter()
        .map(|(name, tally)| type_row(name, tally, &reads.roles))
        .collect();
    if !rest.is_empty() {
        let mut other = Tally::default();
        for (_, tally) in rest {
            other.merge(tally);
        }
        rows.push(type_row(OTHER_KEY.to_owned(), other, &reads.roles));
    }
    Agents {
        attempts: total.attempts,
        ms: total.ms_total(),
        failure_rate: total.failure_rate(),
        pass_rate: total.pass_rate(),
        outcome: total.outcome,
        verified: total.verified,
        by_type: rows,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::{Scope, Window};
    use crate::config::ModelsConfig;
    use crate::devtime::store::agent_fixture::{AgentAttempt, agent, insert_agent, insert_mark};
    use crate::devtime::store::attempt_fixture::{attempt, insert_attempt};
    use crate::devtime::store::fixture::session;
    use chrono::{DateTime, Utc};

    fn row(
        agent_type: Option<&str>,
        outcome: &str,
        bg_status: Option<&str>,
        verified: Option<&str>,
        ms: Option<i64>,
    ) -> AgentAttemptRow {
        AgentAttemptRow {
            agent_type: agent_type.map(str::to_owned),
            outcome: outcome.to_owned(),
            bg_status: bg_status.map(str::to_owned),
            verified: verified.map(str::to_owned),
            ms,
        }
    }

    fn fold(rows: Vec<AgentAttemptRow>) -> Agents {
        block(&prepare(rows, &DevtimeRulesConfig::default()))
    }

    /// An ended, unmarked `ok` attempt of `agent_type`.
    fn ok(agent_type: Option<&str>) -> AgentAttemptRow {
        row(agent_type, "ok", None, None, Some(1000))
    }

    #[test]
    fn empty_window_is_zeroed_with_null_rates() {
        assert_eq!(fold(Vec::new()), Agents::default());
        let empty = fold(Vec::new());
        assert_eq!((empty.attempts, empty.ms), (0, 0));
        assert_eq!(empty.failure_rate, None);
        assert_eq!(empty.pass_rate, None);
        assert!(empty.by_type.is_empty());
    }

    #[test]
    fn outcomes_fold_errors_background_failures_and_interruptions() {
        let t = Some("worker");
        let rows = vec![
            row(t, "ok", None, None, None),
            row(t, "error", None, None, None),
            row(t, "interrupted", None, None, None),
            row(t, "launched", Some("completed"), None, None),
            row(t, "launched", Some("failed"), None, None),
            row(t, "launched", Some("killed"), None, None),
            row(t, "launched", None, None, None),
            row(t, "unknown", None, None, None),
        ];
        let agents = fold(rows);
        assert_eq!(
            agents.outcome,
            AgentOutcomes {
                ok: 2,
                failed: 3,
                interrupted: 1,
                other: 2
            }
        );
        assert_eq!(agents.attempts, 8);
        assert_eq!(agents.failure_rate, Some(3.0 / 8.0));

        let error = fold(vec![row(t, "error", Some("completed"), None, None)]);
        assert_eq!(error.outcome.failed, 1);
        assert_eq!(error.outcome.ok, 0);
    }

    #[test]
    fn verification_counts_marks_and_unruled_attempts() {
        let t = Some("worker");
        let rows = vec![
            row(t, "ok", None, Some("passed"), None),
            row(t, "ok", None, Some("failed"), None),
            row(t, "ok", None, Some("not_measured"), None),
            row(t, "ok", None, None, None),
            row(t, "ok", None, Some("weird"), None),
        ];
        let agents = fold(rows);
        assert_eq!(
            agents.verified,
            AgentVerification {
                passed: 1,
                failed: 1,
                not_measured: 2,
                unruled: 1
            }
        );
        assert_eq!(agents.pass_rate, Some(0.5));

        let unruled = fold(vec![
            row(t, "ok", None, Some("not_measured"), None),
            row(t, "ok", None, None, None),
        ]);
        assert_eq!(unruled.pass_rate, None);
        assert_eq!(unruled.by_type[0].pass_rate, None);
    }

    #[test]
    fn durations_sum_and_median_ended_attempts_only() {
        let t = Some("worker");
        let agents = fold(vec![
            row(t, "ok", None, None, Some(1000)),
            row(t, "ok", None, None, Some(3000)),
            row(t, "ok", None, None, None),
            row(t, "ok", None, None, Some(-5)),
        ]);
        assert_eq!(agents.ms, 4000);
        assert_eq!(agents.by_type[0].ms, 4000);
        assert_eq!(agents.by_type[0].median_ms, Some(1000));

        let even = fold(vec![
            row(t, "ok", None, None, Some(1)),
            row(t, "ok", None, None, Some(2)),
        ]);
        assert_eq!(even.by_type[0].median_ms, Some(1));

        let open = fold(vec![
            row(t, "ok", None, None, None),
            row(t, "ok", None, None, None),
        ]);
        assert_eq!(open.ms, 0);
        assert_eq!(open.by_type[0].ms, 0);
        assert_eq!(open.by_type[0].median_ms, None);
    }

    #[test]
    fn types_sort_by_attempts_then_name_with_unknown_for_null() {
        let mut rows = Vec::new();
        rows.extend((0..3).map(|_| ok(Some("worker"))));
        rows.extend((0..2).map(|_| ok(None)));
        rows.push(ok(Some("a-b")));
        rows.push(ok(Some("Explore")));
        let names: Vec<String> = fold(rows)
            .by_type
            .into_iter()
            .map(|row| row.agent_type)
            .collect();
        assert_eq!(names, ["worker", UNKNOWN, "Explore", "a-b"]);
    }

    #[test]
    fn roles_come_from_the_rules_globs() {
        let rows = [
            Some("code-reviewer"),
            Some("wf-executor"),
            Some("general-purpose"),
            Some("Explore"),
            None,
        ]
        .into_iter()
        .map(ok)
        .collect();
        let agents = fold(rows);
        let role = |name: &str| {
            agents
                .by_type
                .iter()
                .find(|row| row.agent_type == name)
                .unwrap_or_else(|| panic!("{name} in {:?}", agents.by_type))
                .role
        };
        assert_eq!(role("code-reviewer"), Some("reviewer"));
        assert_eq!(role("wf-executor"), Some("implementer"));
        assert_eq!(role("general-purpose"), Some("implementer"));
        assert_eq!(role("Explore"), None);
        assert_eq!(role(UNKNOWN), None);
    }

    #[test]
    fn types_past_twenty_fold_into_other() {
        // `t00` has 22 attempts, ..., `t21` has 1; `t20`'s take 100 ms each, `t21`'s 400 ms.
        let rows = (0..22)
            .flat_map(|n: i64| {
                let name = format!("t{n:02}");
                let ms = match n {
                    20 => 100,
                    21 => 400,
                    _ => 1000,
                };
                (0..22 - n).map(move |_| row(Some(&name), "ok", None, None, Some(ms)))
            })
            .collect();
        let agents = fold(rows);
        assert_eq!(agents.attempts, 253);
        // `LIST_CAP` types are kept, as in every capped list of the overview, plus the fold.
        assert_eq!(agents.by_type.len(), LIST_CAP + 1);
        assert_eq!(agents.by_type[0].agent_type, "t00");
        assert_eq!(agents.by_type[19].agent_type, "t19");
        let other = agents.by_type.last().unwrap();
        assert_eq!(other.agent_type, OTHER_KEY);
        assert_eq!(other.attempts, 3);
        assert_eq!(other.role, None);
        // Over [100, 100, 400].
        assert_eq!(other.median_ms, Some(100));
        assert_eq!(other.ms, 600);
    }

    #[test]
    fn totals_sum_every_type() {
        let agents = fold(vec![
            row(Some("a"), "ok", None, Some("passed"), Some(10)),
            row(Some("a"), "error", None, Some("failed"), Some(20)),
            row(Some("b"), "interrupted", None, None, Some(30)),
            row(Some("b"), "launched", None, Some("not_measured"), None),
            row(Some("b"), "ok", None, Some("passed"), Some(40)),
        ]);
        let rows = &agents.by_type;
        assert_eq!(rows.len(), 2);
        let sum = |f: fn(&AgentTypeRow) -> i64| rows.iter().map(f).sum::<i64>();
        assert_eq!(agents.attempts, sum(|r| r.attempts));
        assert_eq!(agents.ms, sum(|r| r.ms));
        assert_eq!(agents.outcome.ok, sum(|r| r.outcome.ok));
        assert_eq!(agents.outcome.failed, sum(|r| r.outcome.failed));
        assert_eq!(agents.outcome.interrupted, sum(|r| r.outcome.interrupted));
        assert_eq!(agents.outcome.other, sum(|r| r.outcome.other));
        assert_eq!(agents.verified.passed, sum(|r| r.verified.passed));
        assert_eq!(agents.verified.failed, sum(|r| r.verified.failed));
        assert_eq!(
            agents.verified.not_measured,
            sum(|r| r.verified.not_measured)
        );
        assert_eq!(agents.verified.unruled, sum(|r| r.verified.unruled));
        assert_eq!((agents.attempts, agents.ms), (5, 100));
    }

    fn at(moment: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(moment)
            .unwrap()
            .with_timezone(&Utc)
    }

    /// Noon on 2026-10-17: the 7-day window is 2026-10-11 ..= 17, the previous 10-04 ..= 10.
    async fn overview(pool: &sqlx::SqlitePool, scope: Scope<'_>) -> super::super::DevtimeOverview {
        let now = at("2026-10-17T12:00:00Z");
        super::super::devtime_overview(
            pool,
            &ModelsConfig::default(),
            &DevtimeRulesConfig::default(),
            scope,
            Window::new(7, now).unwrap(),
            now,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn overview_scopes_agent_attempts_by_project_and_window() {
        let pool = crate::testdb::fresh_pool().await;
        let start = "2026-10-01T00:00:00.000Z";
        session(&pool, "a1", "alpha", start, None).await;
        session(&pool, "b1", "beta", start, None).await;
        insert_agent(
            &pool,
            agent("a1", Some("Explore"), "2026-10-12T10:00:00.000Z"),
        )
        .await;
        insert_agent(
            &pool,
            agent("b1", Some("Explore"), "2026-10-13T10:00:00.000Z"),
        )
        .await;
        // The previous window, and after now: neither counts.
        insert_agent(
            &pool,
            agent("a1", Some("Explore"), "2026-10-08T10:00:00.000Z"),
        )
        .await;
        insert_agent(
            &pool,
            agent("a1", Some("Explore"), "2026-10-17T13:00:00.000Z"),
        )
        .await;
        // A tool attempt in the window is metric 7's, not this one's.
        insert_attempt(&pool, attempt("a1", "Bash", "2026-10-12T11:00:00.000Z")).await;

        let global = overview(&pool, Scope::Global).await.agents;
        assert_eq!(global.attempts, 2, "{global:?}");

        let alpha = overview(&pool, Scope::Project("alpha")).await.agents;
        assert_eq!(alpha.attempts, 1, "{alpha:?}");
        assert_eq!(alpha.by_type.len(), 1);
        assert_eq!(alpha.by_type[0].agent_type, "Explore");
    }

    #[tokio::test]
    async fn agents_json_carries_no_session_worktree_or_agent_id() {
        let pool = crate::testdb::fresh_pool().await;
        session(
            &pool,
            "sess-secret-10",
            "alpha",
            "2026-10-12T09:00:00.000Z",
            None,
        )
        .await;
        sqlx::query("UPDATE devtime_sessions SET worktree = 'C:/wt/secret'")
            .execute(&pool)
            .await
            .unwrap();
        let executor = AgentAttempt {
            ended_at: Some("2026-10-12T10:05:00.000Z"),
            ..agent(
                "sess-secret-10",
                Some("wf-executor"),
                "2026-10-12T10:00:00.000Z",
            )
        };
        let id = insert_agent(&pool, executor).await;
        insert_mark(&pool, &id, "sess-secret-10", "passed").await;

        let report = serde_json::to_value(overview(&pool, Scope::Global).await).unwrap();
        let block = &report["agents"];
        assert_eq!(block["verified"]["passed"], 1, "{block}");
        let first = &block["by_type"][0];
        assert!(first.is_object(), "{block}");
        for key in ["session_id", "attempt_id", "agent_id"] {
            assert!(first.get(key).is_none(), "{key} in {first}");
        }
        let text = block.to_string();
        for needle in ["sess-secret", "C:/", "/wt", "agent:"] {
            assert!(!text.contains(needle), "{needle} in {text}");
        }
    }
}
