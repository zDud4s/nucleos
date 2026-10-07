//! PURE per-turn active and explained time, and the listing of slow turns that no rule explains.
//! `turn_stats` runs at ingestion over one session's facts; `list_unexplained` reads the stored rows
//! and decides at listing time, because class medians move as data arrives.

use std::collections::{BTreeMap, HashMap};

use sqlx::SqlitePool;

use crate::config::DevtimeRulesConfig;
use crate::devtime_rules::{SessionFacts, Waste, format_ms};
use crate::devtime_store::{self, SpanMark, TurnStatRow};

/// The lane a turn belongs to: subagent and background lanes never count toward a turn's time.
const MAIN_LANE: &str = "main";

/// Span kinds that are the machine or an agent doing work, or waiting on work the session launched.
/// `wait_human` and `idle` are not. The same five kinds `devtime_rules::WORK_KINDS` treats as work.
const ACTIVE_KINDS: [&str; 5] = [
    "model",
    "tool",
    "subagent",
    "wait_background",
    "wait_machine",
];

/// A window longer than this is clamped, so the cutoff arithmetic cannot overflow.
const MAX_WINDOW_DAYS: i64 = 36_500;

/// A turn that took far longer than its class's median and that no rule explains.
#[derive(Debug, Clone, PartialEq)]
#[cfg_attr(not(test), allow(dead_code))] // SP4's endpoint is the reader
pub struct UnexplainedTurn {
    pub stat: TurnStatRow,
    /// The median `active_ms` of the turn's class.
    pub class_median_ms: i64,
}

/// `c{i}`, where `i` is the index of the first bound at or above `calls`, else the number of bounds.
fn turn_class(calls: i64, bounds: &[u32]) -> String {
    let index = bounds
        .iter()
        .position(|bound| i64::from(*bound) >= calls)
        .unwrap_or(bounds.len());
    format!("c{index}")
}

/// Milliseconds of `[from, to)` that lie inside `[start, end)`.
fn overlap_ms(from: i64, to: i64, start: i64, end: i64) -> i64 {
    (to.min(end) - from.max(start)).max(0)
}

/// One row per main-lane turn of the session.
///
/// A turn runs from its own start to the next turn's start, or to the session's end for the last one.
/// `calls` counts the main-lane attempts that started inside it, `active_ms` the part of it covered by
/// main-lane work spans, and `explained_ms` the part of that carried by spans a rule marked `rework`
/// or `avoidable`.
pub fn turn_stats(
    facts: &SessionFacts,
    marks: &[SpanMark],
    cfg: &DevtimeRulesConfig,
) -> Vec<TurnStatRow> {
    let explaining = [Waste::Rework.as_str(), Waste::Avoidable.as_str()];
    let mut explained_spans: HashMap<i64, bool> = HashMap::new();
    for mark in marks {
        if let Some(waste) = mark.waste.as_deref() {
            explained_spans.insert(mark.span_id, explaining.contains(&waste));
        }
    }

    let mut turns: Vec<(i64, i64)> = facts
        .turns
        .iter()
        .map(|turn| (turn.started_ms, turn.seq))
        .collect();
    turns.sort_unstable();

    let mut rows = Vec::with_capacity(turns.len());
    for (index, (start, seq)) in turns.iter().enumerate() {
        let start = *start;
        let end = match turns.get(index + 1) {
            Some((next_start, _)) => *next_start,
            None => facts.ended_ms.max(start),
        };
        let calls = facts
            .attempts
            .iter()
            .filter(|attempt| {
                attempt.is_main && attempt.started_ms >= start && attempt.started_ms < end
            })
            .count();
        let mut active_ms = 0_i64;
        let mut explained_ms = 0_i64;
        for span in &facts.spans {
            if span.lane != MAIN_LANE || !ACTIVE_KINDS.contains(&span.kind.as_str()) {
                continue;
            }
            let covered = overlap_ms(span.started_ms, span.ended_ms, start, end);
            active_ms += covered;
            if explained_spans.get(&span.id).copied().unwrap_or(false) {
                explained_ms += covered;
            }
        }
        let calls = i64::try_from(calls).unwrap_or(i64::MAX);
        rows.push(TurnStatRow {
            session_id: facts.session_id.clone(),
            turn_seq: *seq,
            project_id: facts.project_id.clone(),
            turn_class: turn_class(calls, &cfg.unexplained.class_bounds),
            started_at: format_ms(start),
            calls,
            active_ms,
            explained_ms,
        });
    }
    rows
}

/// The median of a sorted, non-empty slice; the mean of the two middle values (rounded down) when the
/// count is even.
fn median_of_sorted(values: &[i64]) -> i64 {
    let middle = values.len() / 2;
    if values.len() % 2 == 1 {
        values[middle]
    } else {
        (values[middle - 1] + values[middle]) / 2
    }
}

/// PURE: the turns in `window` that are slow for their class and that no rule explains, largest first.
///
/// Medians are taken per (project, class) over the whole window, and only for classes with at least
/// `min_class_turns` turns; a turn in a smaller class is never listed. `since`, when given, hides turns
/// that started earlier (they still count toward the medians).
fn pick_unexplained(
    window: &[TurnStatRow],
    since: Option<&str>,
    cfg: &DevtimeRulesConfig,
) -> Vec<UnexplainedTurn> {
    let settings = &cfg.unexplained;
    let mut by_class: BTreeMap<(&str, &str), Vec<i64>> = BTreeMap::new();
    for row in window {
        by_class
            .entry((row.project_id.as_str(), row.turn_class.as_str()))
            .or_default()
            .push(row.active_ms);
    }
    let min_turns = usize::try_from(settings.min_class_turns).unwrap_or(usize::MAX);
    let mut medians: BTreeMap<(&str, &str), i64> = BTreeMap::new();
    for (class, mut values) in by_class {
        if values.len() < min_turns {
            continue;
        }
        values.sort_unstable();
        medians.insert(class, median_of_sorted(&values));
    }

    let min_active_ms = i64::try_from(settings.min_turn_seconds)
        .unwrap_or(i64::MAX)
        .saturating_mul(1000);
    let mut found = Vec::new();
    for row in window {
        if since.is_some_and(|floor| row.started_at.as_str() < floor) {
            continue;
        }
        let Some(median) = medians
            .get(&(row.project_id.as_str(), row.turn_class.as_str()))
            .copied()
        else {
            continue;
        };
        if row.active_ms <= 0 {
            continue;
        }
        let active = row.active_ms as f64;
        let explained = row.explained_ms as f64;
        let slow = active >= settings.median_multiple * (median as f64);
        let long = row.active_ms >= min_active_ms;
        let unexplained = explained / active <= settings.max_explained_fraction;
        if slow && long && unexplained {
            found.push(UnexplainedTurn {
                stat: row.clone(),
                class_median_ms: median,
            });
        }
    }
    found.sort_by(|a, b| {
        b.stat
            .active_ms
            .cmp(&a.stat.active_ms)
            .then_with(|| a.stat.session_id.cmp(&b.stat.session_id))
            .then_with(|| a.stat.turn_seq.cmp(&b.stat.turn_seq))
    });
    found
}

/// The unexplained turns, largest first, at most `limit`.
///
/// Reads the stored turn stats of the last `unexplained.window_days` days, so the class medians follow
/// the data as it arrives. `since` narrows what is listed, not what the medians are taken over.
#[cfg_attr(not(test), allow(dead_code))] // SP4's endpoint is the reader
pub async fn list_unexplained(
    pool: &SqlitePool,
    cfg: &DevtimeRulesConfig,
    project_id: Option<&str>,
    since: Option<&str>,
    limit: usize,
) -> sqlx::Result<Vec<UnexplainedTurn>> {
    let days = i64::from(cfg.unexplained.window_days).min(MAX_WINDOW_DAYS);
    let cutoff = chrono::Utc::now() - chrono::Duration::days(days);
    let window_start = format_ms(cutoff.timestamp_millis());
    let window = devtime_store::turn_stats_rows(pool, project_id, Some(&window_start)).await?;
    let mut found = pick_unexplained(&window, since, cfg);
    found.truncate(limit);
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devtime_rules::{AttemptFact, Outcome, SpanFact, TurnFact};
    use crate::devtime_rules_fixture::test_pool;
    use crate::devtime_store::{RuleWrite, SessionHead};

    /// Epoch milliseconds of `secs` seconds after an arbitrary origin.
    fn s(secs: i64) -> i64 {
        1_760_000_000_000 + secs * 1000
    }

    fn turn(seq: i64, start_secs: i64) -> TurnFact {
        TurnFact {
            seq,
            started_ms: s(start_secs),
            ended_ms: s(start_secs),
            interrupted: false,
            opens_with_correction: None,
        }
    }

    fn span(id: i64, lane: &str, kind: &str, from_secs: i64, to_secs: i64) -> SpanFact {
        SpanFact {
            id,
            lane: lane.to_string(),
            kind: kind.to_string(),
            started_ms: s(from_secs),
            ended_ms: s(to_secs),
            attempt_ids: Vec::new(),
            context_tokens: None,
        }
    }

    fn attempt(id: &str, lane: &str, start_secs: i64) -> AttemptFact {
        AttemptFact {
            idx: 0,
            attempt_id: id.to_string(),
            lane: lane.to_string(),
            is_main: lane == MAIN_LANE,
            message_id: None,
            turn_seq: None,
            kind: "tool".to_string(),
            tool_name: "Bash".to_string(),
            agent_type: None,
            agent_id: None,
            role: None,
            model: None,
            effort: None,
            started_ms: s(start_secs),
            ended_ms: Some(s(start_secs + 1)),
            done_ms: s(start_secs + 1),
            outcome: Outcome::Ok,
            exit_code: None,
            error_class: None,
            cmd_program: None,
            cmd_hash: None,
            cmd_class: None,
            is_shell: false,
            is_edit: false,
            is_read: false,
            is_search: false,
            mutating: false,
            is_sleep: false,
            is_commit: false,
            timeout_ms: None,
            background: false,
            bg_status: None,
            files: Vec::new(),
            edits: Vec::new(),
            reads: Vec::new(),
            refs_in: Vec::new(),
            refs_out: Vec::new(),
            refs_known: true,
        }
    }

    fn mark(span_id: i64, waste: &str) -> SpanMark {
        SpanMark {
            span_id,
            waste: Some(waste.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn turn_class_by_bounds() {
        let bounds = [0_u32, 5, 20];
        assert_eq!(turn_class(0, &bounds), "c0");
        assert_eq!(turn_class(1, &bounds), "c1");
        assert_eq!(turn_class(5, &bounds), "c1");
        assert_eq!(turn_class(6, &bounds), "c2");
        assert_eq!(turn_class(20, &bounds), "c2");
        assert_eq!(turn_class(21, &bounds), "c3");
        assert_eq!(turn_class(1000, &bounds), "c3");
        // No bounds at all: one class.
        assert_eq!(turn_class(7, &[]), "c0");
    }

    #[test]
    fn turn_stats_active_and_explained() {
        let facts = SessionFacts {
            session_id: "s1".to_string(),
            project_id: "p1".to_string(),
            started_ms: s(0),
            ended_ms: s(200),
            turns: vec![turn(2, 100), turn(1, 0)],
            attempts: vec![
                attempt("a1", "main", 5),
                attempt("a2", "main", 20),
                attempt("a3", "main", 70),
                attempt("a4", "main", 120),
                attempt("a5", "agent:x", 130),
            ],
            spans: vec![
                span(1, "main", "model", 0, 10),
                span(2, "main", "tool", 10, 40),
                span(3, "main", "wait_human", 40, 60),
                span(4, "main", "tool", 60, 130),
                span(5, "bg:toolu_a", "tool", 10, 90),
                span(6, "main", "idle", 130, 150),
                span(7, "main", "subagent", 150, 200),
            ],
            ..Default::default()
        };
        let marks = vec![
            mark(1, "useful"),
            mark(2, "rework"),
            mark(4, "avoidable"),
            mark(7, "useful"),
        ];
        let rows = turn_stats(&facts, &marks, &DevtimeRulesConfig::default());
        assert_eq!(rows.len(), 2);

        // Turn 1 is [0, 100): 10 s model, 30 s rework tool, 40 s of the avoidable tool.
        assert_eq!(rows[0].session_id, "s1");
        assert_eq!(rows[0].project_id, "p1");
        assert_eq!(rows[0].turn_seq, 1);
        assert_eq!(rows[0].started_at, format_ms(s(0)));
        assert_eq!(rows[0].calls, 3);
        assert_eq!(rows[0].turn_class, "c1");
        assert_eq!(rows[0].active_ms, 80_000);
        assert_eq!(rows[0].explained_ms, 70_000);

        // Turn 2 is [100, 200): 30 s of the avoidable tool, then idle, then 50 s of subagent.
        assert_eq!(rows[1].turn_seq, 2);
        assert_eq!(rows[1].started_at, format_ms(s(100)));
        assert_eq!(
            rows[1].calls, 1,
            "the subagent lane's attempt is not a main call"
        );
        assert_eq!(rows[1].active_ms, 80_000);
        assert_eq!(rows[1].explained_ms, 30_000);
    }

    #[test]
    fn turn_stats_of_a_session_without_turns_is_empty() {
        let facts = SessionFacts::default();
        assert!(turn_stats(&facts, &[], &DevtimeRulesConfig::default()).is_empty());
    }

    /// A timestamp inside the default 30-day window.
    fn recent() -> String {
        format_ms(chrono::Utc::now().timestamp_millis() - 86_400_000)
    }

    fn stat(seq: i64, class: &str, active_ms: i64, explained_ms: i64) -> TurnStatRow {
        TurnStatRow {
            session_id: "s1".to_string(),
            turn_seq: seq,
            project_id: "p1".to_string(),
            turn_class: class.to_string(),
            started_at: recent(),
            calls: 3,
            active_ms,
            explained_ms,
        }
    }

    /// Replaces the stored turn stats of session `s1` with `rows`, through the store's own writer.
    async fn seed(pool: &SqlitePool, rows: Vec<TurnStatRow>) {
        let head = SessionHead {
            session_id: "s1".to_string(),
            project_id: "p1".to_string(),
            updated_at: "2026-10-04T10:00:00.000Z".to_string(),
            ..Default::default()
        };
        let write = RuleWrite {
            turn_stats: rows,
            ..Default::default()
        };
        devtime_store::write_rule_results(pool, &head, &write, "fp")
            .await
            .unwrap();
    }

    /// `n` ordinary turns of a class, numbered from `first_seq`.
    fn ordinary(first_seq: i64, n: i64, class: &str) -> Vec<TurnStatRow> {
        (first_seq..first_seq + n)
            .map(|seq| stat(seq, class, 100_000, 0))
            .collect()
    }

    #[tokio::test]
    async fn list_unexplained_largest_first_above_class_median() {
        let pool = test_pool().await;
        let cfg = DevtimeRulesConfig::default();
        let mut rows = ordinary(1, 12, "c1");
        rows.push(stat(13, "c1", 400_000, 0)); // 4x the median, 400 s: listed
        rows.push(stat(14, "c1", 600_000, 50_000)); // 6x the median, 8% explained: listed first
        rows.push(stat(15, "c1", 250_000, 0)); // under 5 minutes: not listed
        rows.push(stat(16, "c1", 500_000, 300_000)); // 60% explained: not listed
        seed(&pool, rows).await;

        let found = list_unexplained(&pool, &cfg, Some("p1"), None, 10)
            .await
            .unwrap();
        let seqs: Vec<i64> = found.iter().map(|turn| turn.stat.turn_seq).collect();
        assert_eq!(seqs, vec![14, 13]);
        assert_eq!(found[0].class_median_ms, 100_000);
        assert_eq!(found[0].stat.active_ms, 600_000);

        let one = list_unexplained(&pool, &cfg, None, None, 1).await.unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].stat.turn_seq, 14);

        // Another project's filter finds nothing, and a `since` after the turns hides them.
        let other = list_unexplained(&pool, &cfg, Some("p2"), None, 10)
            .await
            .unwrap();
        assert!(other.is_empty());
        let later = format_ms(chrono::Utc::now().timestamp_millis() + 86_400_000);
        let hidden = list_unexplained(&pool, &cfg, None, Some(&later), 10)
            .await
            .unwrap();
        assert!(hidden.is_empty());
    }

    #[tokio::test]
    async fn list_unexplained_skips_small_classes_and_explained_turns() {
        let pool = test_pool().await;
        let cfg = DevtimeRulesConfig::default();

        // Class c2 has nine turns, one below min_class_turns: it has no median, so nothing in it lists.
        let mut rows = ordinary(1, 8, "c2");
        rows.push(stat(9, "c2", 900_000, 0));
        // Class c1 is large enough, but its only slow turn is mostly explained by a rule.
        rows.extend(ordinary(10, 12, "c1"));
        rows.push(stat(22, "c1", 500_000, 300_000));
        seed(&pool, rows.clone()).await;
        let found = list_unexplained(&pool, &cfg, None, None, 10).await.unwrap();
        assert!(found.is_empty(), "got {found:?}");

        // One more turn in c2 reaches the minimum, and the slow one is listed.
        rows.push(stat(23, "c2", 100_000, 0));
        seed(&pool, rows).await;
        let found = list_unexplained(&pool, &cfg, None, None, 10).await.unwrap();
        let seqs: Vec<i64> = found.iter().map(|turn| turn.stat.turn_seq).collect();
        assert_eq!(seqs, vec![9]);
        assert_eq!(found[0].class_median_ms, 100_000);
    }
}
