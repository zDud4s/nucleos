//! Family D, context group: path and sequence rules over reads, edits and files (D1 THRASH, D2, D3
//! FLAILING, D10, D13 UNVERIFIED, D14 LATE SCOPE). Rules read the session's facts and the thresholds in
//! `DevtimeRulesConfig`; they never write a row. A finding carries ids, times and counts only: never a
//! path's content, a message or a command line.
//!
//! D1, D2, D10, D13 and D14 look at paths and skip the external ones (scratch, logs: `paths.external`).
//! D3 never can: a search's input is not stored, so there is no path to exclude.

use std::collections::{BTreeMap, BTreeSet};

use crate::config::DevtimeRulesConfig;
use crate::devtime_rules::{
    AttemptFact, Confidence, Finding, Outcome, RuleOutput, SessionFacts, SpanPolicy, duration,
    lane_attempts, passed, path_edited_between,
};
use crate::devtime_rules_cmd::CmdClass;

pub fn run(facts: &SessionFacts, cfg: &DevtimeRulesConfig) -> RuleOutput {
    let mut out = RuleOutput::default();
    out.findings.extend(d1_thrash(facts, cfg));
    out.findings.extend(d2_reread(facts));
    out.findings.extend(d3_flailing(facts, cfg));
    out.findings.extend(d10_reread_after_compact(facts));
    out.findings.extend(d13_unverified(facts));
    out.findings.extend(d14_late_scope(facts, cfg));
    out
}

// --- shared small helpers ---------------------------------------------------------------------------

/// An attempt that changed the working tree and succeeded.
fn is_ok_edit(a: &AttemptFact) -> bool {
    (a.is_edit || a.mutating) && a.outcome == Outcome::Ok
}

/// The paths an edit attempt wrote: its edit records, else its written files.
fn edited_paths(a: &AttemptFact) -> Vec<&str> {
    if a.edits.is_empty() {
        a.files.iter().map(String::as_str).collect()
    } else {
        a.edits.iter().map(|edit| edit.path.as_str()).collect()
    }
}

/// The latest `done_ms` among `attempts`, or `fallback` when there are none.
fn last_done(attempts: &[&AttemptFact], fallback: i64) -> i64 {
    attempts.iter().map(|a| a.done_ms).max().unwrap_or(fallback)
}

/// The attempts in time order, without repeats.
fn unique_by_time(mut attempts: Vec<&AttemptFact>) -> Vec<&AttemptFact> {
    attempts.sort_by(|a, b| {
        (a.started_ms, a.attempt_id.as_str()).cmp(&(b.started_ms, b.attempt_id.as_str()))
    });
    attempts.dedup_by(|a, b| a.attempt_id == b.attempt_id);
    attempts
}

// --- D1 / THRASH ------------------------------------------------------------------------------------

/// One run of reads of the same (path, offset) in a lane.
struct Streak {
    count: usize,
    first_ms: i64,
    last_idx: usize,
    last_ms: i64,
    last_done_ms: i64,
    cost_ms: i64,
    /// The reads after the first one.
    ids: Vec<String>,
}

impl Streak {
    /// A streak whose first read is attempt `a`, the `call_idx`-th call of its lane.
    fn begin(a: &AttemptFact, call_idx: usize) -> Streak {
        Streak {
            count: 1,
            first_ms: a.started_ms,
            last_idx: call_idx,
            last_ms: a.started_ms,
            last_done_ms: a.done_ms,
            cost_ms: 0,
            ids: Vec::new(),
        }
    }

    /// One more read of the same key.
    fn extend(&mut self, a: &AttemptFact, call_idx: usize) {
        self.count += 1;
        if self.ids.last() != Some(&a.attempt_id) {
            self.ids.push(a.attempt_id.clone());
        }
        self.cost_ms += duration(a);
        self.last_idx = call_idx;
        self.last_ms = a.started_ms;
        self.last_done_ms = a.done_ms;
    }
}

fn thrash_finding(lane: &str, streak: &Streak, min_repeats: usize) -> Option<Finding> {
    if streak.count < min_repeats || streak.ids.is_empty() {
        return None;
    }
    Some(Finding {
        rule_id: "D1",
        lane: lane.to_string(),
        started_ms: streak.first_ms,
        ended_ms: streak.last_done_ms.max(streak.first_ms),
        cost_ms: streak.cost_ms,
        attempt_ids: streak.ids.clone(),
        confidence: Confidence::Exact,
        count: streak.count as i64,
        policy: SpanPolicy::Attempts,
    })
}

/// The same (path, offset) read `thrash_repeats` times or more, with no edit of the path in between
/// and at most `thrash_reset_calls` calls between two consecutive reads. Other offsets are paging.
fn d1_thrash(facts: &SessionFacts, cfg: &DevtimeRulesConfig) -> Vec<Finding> {
    let min_repeats = cfg.thresholds.thrash_repeats as usize;
    let reset_calls = cfg.thresholds.thrash_reset_calls as usize;
    let lanes: BTreeSet<&str> = facts.attempts.iter().map(|a| a.lane.as_str()).collect();
    let mut findings = Vec::new();
    for lane in lanes {
        let mut streaks: BTreeMap<(String, i64), Streak> = BTreeMap::new();
        for (call_idx, a) in lane_attempts(facts, lane).enumerate() {
            for read in &a.reads {
                if facts.paths.is_external(&read.path) {
                    continue;
                }
                let key = (read.path.clone(), read.offset);
                match streaks.remove(&key) {
                    None => {
                        streaks.insert(key, Streak::begin(a, call_idx));
                    }
                    Some(mut held) => {
                        let continues = call_idx.saturating_sub(held.last_idx) <= reset_calls
                            && !path_edited_between(facts, &read.path, held.last_ms, a.started_ms);
                        if continues {
                            held.extend(a, call_idx);
                            streaks.insert(key, held);
                        } else {
                            findings.extend(thrash_finding(lane, &held, min_repeats));
                            streaks.insert(key, Streak::begin(a, call_idx));
                        }
                    }
                }
            }
        }
        for (_, held) in std::mem::take(&mut streaks) {
            findings.extend(thrash_finding(lane, &held, min_repeats));
        }
    }
    findings.sort_by(|a, b| {
        (a.started_ms, a.attempt_ids.first()).cmp(&(b.started_ms, b.attempt_ids.first()))
    });
    findings
}

// --- D2 ---------------------------------------------------------------------------------------------

/// The controller reads a path a subagent already read, and nobody edited it since.
fn d2_reread(facts: &SessionFacts) -> Vec<Finding> {
    let mut findings = Vec::new();
    for r in facts.attempts.iter().filter(|a| a.is_main) {
        for read in &r.reads {
            if facts.paths.is_external(&read.path) {
                continue;
            }
            // The latest subagent read of the path that finished before this one started. Any earlier
            // one has a longer window to the controller's read, so it cannot qualify when this does not.
            let sub_done = facts
                .attempts
                .iter()
                .filter(|s| {
                    s.lane.starts_with("agent:")
                        && s.done_ms < r.started_ms
                        && s.reads.iter().any(|sr| sr.path == read.path)
                })
                .map(|s| s.done_ms)
                .max();
            let Some(sub_done) = sub_done else {
                continue;
            };
            if path_edited_between(facts, &read.path, sub_done, r.started_ms) {
                continue;
            }
            findings.push(Finding {
                rule_id: "D2",
                lane: r.lane.clone(),
                started_ms: r.started_ms,
                ended_ms: r.done_ms.max(r.started_ms),
                cost_ms: duration(r),
                attempt_ids: vec![r.attempt_id.clone()],
                confidence: Confidence::Exact,
                count: 1,
                policy: SpanPolicy::Attempts,
            });
            break;
        }
    }
    findings
}

// --- D3 / FLAILING ----------------------------------------------------------------------------------

fn streak_finding(streak: &[&AttemptFact], confidence: Confidence) -> Finding {
    let started_ms = streak.first().map(|a| a.started_ms).unwrap_or(0);
    let ended_ms = last_done(streak, started_ms).max(started_ms);
    Finding {
        rule_id: "D3",
        lane: "main".to_string(),
        started_ms,
        ended_ms,
        cost_ms: ended_ms - started_ms,
        attempt_ids: streak.iter().map(|a| a.attempt_id.clone()).collect(),
        confidence,
        count: streak.len() as i64,
        policy: SpanPolicy::Interval,
    }
}

/// Ends the flail streak (searches with no Read or edit). When it is long enough it is a finding, and its
/// searches leave the burst so the burst cannot report the same calls again.
fn end_flail<'a>(
    flail: &mut Vec<&'a AttemptFact>,
    burst: &mut Vec<&'a AttemptFact>,
    min_flail: usize,
    out: &mut Vec<Finding>,
) {
    if !flail.is_empty() && flail.len() >= min_flail {
        out.push(streak_finding(flail.as_slice(), Confidence::Exact));
        burst.retain(|b| !flail.iter().any(|f| f.attempt_id == b.attempt_id));
    }
    flail.clear();
}

/// Ends the burst streak (searches with no edit).
fn end_burst(burst: &mut Vec<&AttemptFact>, min_burst: usize, out: &mut Vec<Finding>) {
    if !burst.is_empty() && burst.len() >= min_burst {
        out.push(streak_finding(burst.as_slice(), Confidence::Inferred));
    }
    burst.clear();
}

/// Main-lane searches piling up: `d3_flailing_burst` of them without a Read or an edit (exact), or
/// `d3_search_burst` without an edit (inferred).
fn d3_flailing(facts: &SessionFacts, cfg: &DevtimeRulesConfig) -> Vec<Finding> {
    let min_flail = cfg.thresholds.d3_flailing_burst as usize;
    let min_burst = cfg.thresholds.d3_search_burst as usize;
    let mut findings = Vec::new();
    let mut flail: Vec<&AttemptFact> = Vec::new();
    let mut burst: Vec<&AttemptFact> = Vec::new();
    for a in lane_attempts(facts, "main") {
        if a.is_search {
            flail.push(a);
            burst.push(a);
        } else if is_ok_edit(a) {
            end_flail(&mut flail, &mut burst, min_flail, &mut findings);
            end_burst(&mut burst, min_burst, &mut findings);
        } else if a.is_read {
            end_flail(&mut flail, &mut burst, min_flail, &mut findings);
        }
    }
    end_flail(&mut flail, &mut burst, min_flail, &mut findings);
    end_burst(&mut burst, min_burst, &mut findings);
    findings
}

// --- D10 --------------------------------------------------------------------------------------------

/// After a compaction, the first re-read of each path the lane had already read before it (and nobody
/// edited since). One finding per compaction.
fn d10_reread_after_compact(facts: &SessionFacts) -> Vec<Finding> {
    let mut findings = Vec::new();
    for marker in facts
        .markers
        .iter()
        .filter(|m| m.kind == "compact_boundary")
    {
        let lane = marker.lane.as_str();
        let next_marker_ms = facts
            .markers
            .iter()
            .filter(|m| m.kind == "compact_boundary" && m.lane == marker.lane)
            .map(|m| m.ts_ms)
            .filter(|ts| *ts > marker.ts_ms)
            .min()
            .unwrap_or(i64::MAX);

        // path -> the start of the last read before the compaction
        let mut before: BTreeMap<&str, i64> = BTreeMap::new();
        for a in lane_attempts(facts, lane).filter(|a| a.started_ms < marker.ts_ms) {
            for read in &a.reads {
                if !facts.paths.is_external(&read.path) {
                    before.insert(read.path.as_str(), a.started_ms);
                }
            }
        }

        // path -> the first read after the compaction (and before the next one)
        let mut after: BTreeMap<&str, &AttemptFact> = BTreeMap::new();
        for a in lane_attempts(facts, lane)
            .filter(|a| a.started_ms > marker.ts_ms && a.started_ms < next_marker_ms)
        {
            for read in &a.reads {
                if before.contains_key(read.path.as_str()) {
                    after.entry(read.path.as_str()).or_insert(a);
                }
            }
        }

        let mut reads: Vec<&AttemptFact> = Vec::new();
        let mut paths = 0_i64;
        for (path, a) in after.iter() {
            let seen_ms = before.get(*path).copied().unwrap_or(i64::MIN);
            if path_edited_between(facts, path, seen_ms, a.started_ms) {
                continue;
            }
            paths += 1;
            reads.push(*a);
        }
        let reads = unique_by_time(reads);
        let Some(first) = reads.first() else {
            continue;
        };
        findings.push(Finding {
            rule_id: "D10",
            lane: marker.lane.clone(),
            started_ms: first.started_ms,
            ended_ms: last_done(&reads, first.started_ms).max(first.started_ms),
            cost_ms: reads.iter().map(|a| duration(a)).sum(),
            attempt_ids: reads.iter().map(|a| a.attempt_id.clone()).collect(),
            confidence: Confidence::Exact,
            count: paths,
            policy: SpanPolicy::Attempts,
        });
    }
    findings
}

// --- D13 / UNVERIFIED -------------------------------------------------------------------------------

/// Code edited after the last passing test or build (or with none at all). A flag on the session.
fn d13_unverified(facts: &SessionFacts) -> Vec<Finding> {
    let last_green = facts
        .attempts
        .iter()
        .filter(|a| {
            a.is_shell
                && matches!(a.cmd_class, Some(CmdClass::Test) | Some(CmdClass::Build))
                && passed(a)
        })
        .map(|a| a.done_ms)
        .max();
    let edits: Vec<&AttemptFact> = facts
        .attempts
        .iter()
        .filter(|a| {
            is_ok_edit(a)
                && last_green.is_none_or(|green| a.started_ms > green)
                && edited_paths(a)
                    .iter()
                    .any(|p| !facts.paths.is_external(p) && facts.paths.is_code(p))
        })
        .collect();
    let Some(first) = edits.first() else {
        return Vec::new();
    };
    vec![Finding {
        rule_id: "D13",
        lane: first.lane.clone(),
        started_ms: first.started_ms,
        ended_ms: first.started_ms,
        cost_ms: 0,
        attempt_ids: edits.iter().map(|a| a.attempt_id.clone()).collect(),
        confidence: Confidence::Exact,
        count: edits.len() as i64,
        policy: SpanPolicy::Signal,
    }]
}

// --- D14 / LATE SCOPE -------------------------------------------------------------------------------

/// Many files first touched in the last fraction of the session: the scope arrived late. A flag on the
/// session.
fn d14_late_scope(facts: &SessionFacts, cfg: &DevtimeRulesConfig) -> Vec<Finding> {
    let span = facts.ended_ms - facts.started_ms;
    if span <= 0 {
        return Vec::new();
    }
    let fraction = cfg.thresholds.late_scope_fraction;
    let threshold = facts.started_ms as f64 + (1.0 - fraction) * (span as f64);
    let min_files = cfg.thresholds.late_scope_min_files as usize;

    let mut first_edit: BTreeMap<&str, &AttemptFact> = BTreeMap::new();
    for a in facts.attempts.iter().filter(|a| is_ok_edit(a)) {
        for path in edited_paths(a) {
            if !facts.paths.is_external(path) {
                first_edit.entry(path).or_insert(a);
            }
        }
    }
    let late: Vec<&AttemptFact> = first_edit
        .values()
        .copied()
        .filter(|a| (a.started_ms as f64) >= threshold)
        .collect();
    if late.is_empty() || late.len() < min_files {
        return Vec::new();
    }
    let files = late.len() as i64;
    let late = unique_by_time(late);
    let Some(first) = late.first() else {
        return Vec::new();
    };
    vec![Finding {
        rule_id: "D14",
        lane: first.lane.clone(),
        started_ms: first.started_ms,
        ended_ms: first.started_ms,
        cost_ms: 0,
        attempt_ids: late.iter().map(|a| a.attempt_id.clone()).collect(),
        confidence: Confidence::Exact,
        count: files,
        policy: SpanPolicy::Signal,
    }]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devtime_rules_fixture::script;

    fn out_of(text: &str) -> RuleOutput {
        let facts = script(text).facts(&DevtimeRulesConfig::default());
        run(&facts, &DevtimeRulesConfig::default())
    }

    fn found<'a>(out: &'a RuleOutput, rule_id: &str) -> Vec<&'a Finding> {
        out.findings
            .iter()
            .filter(|f| f.rule_id == rule_id)
            .collect()
    }

    // --- D1 ---

    #[test]
    fn d1_thrash_fires_on_three_same_offset_reads() {
        let out = out_of(
            "
            turn main 0
            read main 2 path=core/src/a.rs aid=r1
            read main 4 path=core/src/a.rs aid=r2
            read main 6 path=core/src/a.rs aid=r3
        ",
        );
        let hits = found(&out, "D1");
        assert_eq!(hits.len(), 1);
        assert_eq!(
            hits[0].attempt_ids,
            vec!["r2".to_string(), "r3".to_string()]
        );
        assert_eq!(hits[0].count, 3);
        assert_eq!(hits[0].confidence, Confidence::Exact);
    }

    #[test]
    fn d1_silent_on_paging() {
        let out = out_of(
            "
            turn main 0
            read main 2 path=core/src/a.rs off=0
            read main 4 path=core/src/a.rs off=200
            read main 6 path=core/src/a.rs off=400
        ",
        );
        assert!(found(&out, "D1").is_empty());
    }

    #[test]
    fn d1_silent_when_an_edit_intervenes() {
        let out = out_of(
            "
            turn main 0
            read main 2 path=core/src/a.rs
            edit main 4 path=core/src/a.rs
            read main 6 path=core/src/a.rs
            read main 8 path=core/src/a.rs
        ",
        );
        assert!(found(&out, "D1").is_empty());
    }

    #[test]
    fn d1_reset_after_forty_calls() {
        let mut text = String::from("turn main 0\nread main 2 path=core/src/a.rs\n");
        for n in 0..41 {
            text.push_str(&format!("bash main {} prog=\"ls\"\n", 3 + n));
        }
        text.push_str("read main 60 path=core/src/a.rs\nread main 62 path=core/src/a.rs\n");
        let out = out_of(&text);
        assert!(found(&out, "D1").is_empty());
    }

    #[test]
    fn d1_external_path_ignored() {
        let out = out_of(
            "
            turn main 0
            read main 2 path=C:/Users/me/scratchpad/n.txt
            read main 4 path=C:/Users/me/scratchpad/n.txt
            read main 6 path=C:/Users/me/scratchpad/n.txt
        ",
        );
        assert!(found(&out, "D1").is_empty());
    }

    // --- D2 ---

    #[test]
    fn d2_fires_when_the_controller_rereads_a_subagent_read() {
        let out = out_of(
            "
            turn main 0
            read agent:x1 3+1 path=core/src/a.rs aid=sub
            read main 10 path=core/src/a.rs aid=ctl
        ",
        );
        let hits = found(&out, "D2");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].attempt_ids, vec!["ctl".to_string()]);
        assert_eq!(hits[0].confidence, Confidence::Exact);
    }

    #[test]
    fn d2_silent_when_the_controller_reads_first() {
        let out = out_of(
            "
            turn main 0
            read main 3 path=core/src/a.rs
            read agent:x1 10+1 path=core/src/a.rs
        ",
        );
        assert!(found(&out, "D2").is_empty());
    }

    #[test]
    fn d2_silent_when_the_path_was_edited_between() {
        let out = out_of(
            "
            turn main 0
            read agent:x1 3+1 path=core/src/a.rs
            edit main 6 path=core/src/a.rs
            read main 10 path=core/src/a.rs
        ",
        );
        assert!(found(&out, "D2").is_empty());
    }

    #[test]
    fn d2_external_path_ignored() {
        let out = out_of(
            "
            turn main 0
            read agent:x1 3+1 path=C:/Users/me/scratchpad/n.txt
            read main 10 path=C:/Users/me/scratchpad/n.txt
        ",
        );
        assert!(found(&out, "D2").is_empty());
    }

    // --- D3 ---

    #[test]
    fn d3_flailing_fires_exact() {
        let out = out_of(
            "
            turn main 0
            grep main 2 aid=g1
            grep main 4 aid=g2
            glob main 6 aid=g3
            grep main 8 aid=g4
        ",
        );
        let hits = found(&out, "D3");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].confidence, Confidence::Exact);
        assert_eq!(hits[0].count, 4);
        assert_eq!(hits[0].attempt_ids[0], "g1");
    }

    #[test]
    fn d3_burst_fires_inferred() {
        let out = out_of(
            "
            turn main 0
            grep main 2
            grep main 4
            grep main 6
            read main 8 path=core/src/a.rs
            grep main 10
            grep main 12
            grep main 14
        ",
        );
        let hits = found(&out, "D3");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].confidence, Confidence::Inferred);
        assert_eq!(hits[0].count, 6);
    }

    #[test]
    fn d3_silent_when_reads_interleave() {
        let out = out_of(
            "
            turn main 0
            grep main 2
            grep main 4
            read main 6 path=core/src/a.rs
            grep main 8
            grep main 10
        ",
        );
        assert!(found(&out, "D3").is_empty());
    }

    #[test]
    fn d3_silent_when_an_edit_ends_each_streak() {
        let out = out_of(
            "
            turn main 0
            grep main 2
            grep main 4
            grep main 6
            edit main 8 path=core/src/a.rs
            grep main 10
            grep main 12
            grep main 14
        ",
        );
        assert!(found(&out, "D3").is_empty());
    }

    // --- D10 ---

    #[test]
    fn d10_fires_on_a_reread_after_compaction() {
        let out = out_of(
            "
            turn main 0
            read main 2 path=core/src/a.rs aid=before
            marker main 10 kind=compact_boundary
            read main 14 path=core/src/a.rs aid=after
        ",
        );
        let hits = found(&out, "D10");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].attempt_ids, vec!["after".to_string()]);
        assert_eq!(hits[0].count, 1);
    }

    #[test]
    fn d10_silent_on_a_new_path_after_compaction() {
        let out = out_of(
            "
            turn main 0
            read main 2 path=core/src/a.rs
            marker main 10 kind=compact_boundary
            read main 14 path=core/src/b.rs
        ",
        );
        assert!(found(&out, "D10").is_empty());
    }

    #[test]
    fn d10_silent_when_the_path_was_edited() {
        let out = out_of(
            "
            turn main 0
            read main 2 path=core/src/a.rs
            edit main 6 path=core/src/a.rs
            marker main 10 kind=compact_boundary
            read main 14 path=core/src/a.rs
        ",
        );
        assert!(found(&out, "D10").is_empty());
    }

    // --- D13 ---

    #[test]
    fn d13_fires_on_an_edit_after_the_last_green() {
        let out = out_of(
            "
            turn main 0
            bash main 2+2 prog=\"cargo test\" aid=green
            edit main 10 path=core/src/a.rs aid=e1
        ",
        );
        let hits = found(&out, "D13");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].attempt_ids, vec!["e1".to_string()]);
        assert_eq!(hits[0].count, 1);
        assert!(matches!(hits[0].policy, SpanPolicy::Signal));
        assert_eq!(hits[0].cost_ms, 0);
    }

    #[test]
    fn d13_fires_when_the_only_test_failed() {
        let out = out_of(
            "
            turn main 0
            edit main 2 path=core/src/a.rs
            bash main 6+2 prog=\"cargo test\" out=error exit=101
        ",
        );
        assert_eq!(found(&out, "D13").len(), 1);
    }

    #[test]
    fn d13_silent_after_a_green_test() {
        let out = out_of(
            "
            turn main 0
            edit main 2 path=core/src/a.rs
            bash main 6+2 prog=\"cargo test\"
        ",
        );
        assert!(found(&out, "D13").is_empty());
    }

    #[test]
    fn d13_silent_without_edits() {
        let out = out_of(
            "
            turn main 0
            read main 2 path=core/src/a.rs
        ",
        );
        assert!(found(&out, "D13").is_empty());
    }

    #[test]
    fn d13_non_code_ignored() {
        let out = out_of(
            "
            turn main 0
            edit main 2 path=README.md
            edit main 4 path=C:/Users/me/scratchpad/n.rs
        ",
        );
        assert!(found(&out, "D13").is_empty());
    }

    // --- D14 ---

    #[test]
    fn d14_fires_on_three_late_files() {
        let out = out_of(
            "
            turn main 0
            edit main 5 path=core/src/early.rs
            edit main 80 path=core/src/l1.rs aid=l1
            edit main 85 path=core/src/l2.rs aid=l2
            edit main 90 path=core/src/l3.rs aid=l3
            turn main 100
        ",
        );
        let hits = found(&out, "D14");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].count, 3);
        assert_eq!(
            hits[0].attempt_ids,
            vec!["l1".to_string(), "l2".to_string(), "l3".to_string()]
        );
        assert!(matches!(hits[0].policy, SpanPolicy::Signal));
    }

    #[test]
    fn d14_silent_with_two_late_files() {
        let out = out_of(
            "
            turn main 0
            edit main 5 path=core/src/early.rs
            edit main 85 path=core/src/l2.rs
            edit main 90 path=core/src/l3.rs
            turn main 100
        ",
        );
        assert!(found(&out, "D14").is_empty());
    }

    #[test]
    fn d14_silent_when_the_late_edits_revisit_old_files() {
        let out = out_of(
            "
            turn main 0
            edit main 5 path=core/src/a.rs
            edit main 6 path=core/src/b.rs
            edit main 7 path=core/src/c.rs
            edit main 80 path=core/src/a.rs
            edit main 85 path=core/src/b.rs
            edit main 90 path=core/src/c.rs
            turn main 100
        ",
        );
        assert!(found(&out, "D14").is_empty());
    }
}
