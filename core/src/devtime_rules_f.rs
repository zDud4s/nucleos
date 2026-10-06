//! Family F, across sessions (F1, F2): command sequences and failures that recur in several sessions
//! of one project, which no single session can show. PURE over the rows `devtime_store::cross_session_rows`
//! returns; the engine stores what it finds with `scope = 'cross'` and never projects it onto spans.
//!
//! - F1: the same run of `f_sequence_len` or more shell `cmd_hash`es, or of Read paths, in at least
//!   `f_min_sessions` distinct sessions. Adjacent qualifying n-grams that span exactly the same sessions
//!   merge into one longer sequence.
//! - F2: the same `(error_class, cmd_program)` among failed shell attempts in at least `f_min_sessions`
//!   distinct sessions.
//!
//! Nothing here keeps text: a finding carries session ids, a hash of the sequence and, for F2, the
//! closed error class and the program name (spec §8). The window (`f_window_days`) is applied by the
//! caller's query, so the rows arrive already inside it.

use std::collections::{BTreeMap, BTreeSet};

use chrono::DateTime;

use crate::config::DevtimeRulesConfig;
use crate::devtime_rules::sha16;
use crate::devtime_rules_cmd::PathMatcher;
use crate::devtime_store::CrossAttempt;

/// A recurring pattern, with the sessions it spans. Times are epoch milliseconds.
#[derive(Debug, Clone)]
pub struct CrossFinding {
    pub rule_id: &'static str,
    pub sessions: Vec<String>,
    pub latest_session: String,
    pub started_ms: i64,
    pub ended_ms: i64,
    pub cost_ms: i64,
    pub count: i64,
    pub attempt_ids: Vec<String>,
    /// The sequence, or the `(class, program)` key, the finding is anchored on.
    pub anchor: String,
}

/// Closed storage vocabulary: the `outcome` of a failed attempt (`devtime_store::OUTCOMES`).
const OUTCOME_ERROR: &str = "error";
/// What tells the two kinds of F1 sequence apart inside the anchor hash, and the item separator there
/// (the ASCII unit separator, which no path or hash holds).
const KIND_CMD: &str = "cmd";
const KIND_READ: &str = "read";
const SEP: &str = "\u{1f}";

/// One element of a session's ordered sequence: a command hash or a read path, with the interval of
/// the attempt it came from (several reads of one attempt share its interval).
struct Item {
    key: String,
    started_ms: i64,
    done_ms: i64,
}

/// An attempt's interval in epoch milliseconds; `None` when its start does not parse (such a row is
/// dropped, never a panic). A missing or unparseable end falls back to the start.
fn interval_of(row: &CrossAttempt) -> Option<(i64, i64)> {
    let started = parse_ms(&row.started_at)?;
    let done = row
        .ended_at
        .as_deref()
        .and_then(parse_ms)
        .unwrap_or(started)
        .max(started);
    Some((started, done))
}

fn parse_ms(text: &str) -> Option<i64> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|moment| moment.timestamp_millis())
}

fn is_shell(row: &CrossAttempt, cfg: &DevtimeRulesConfig) -> bool {
    cfg.commands
        .shell_tools
        .iter()
        .any(|tool| tool.eq_ignore_ascii_case(&row.tool_name))
}

/// The paths of a `reads` JSON array of `{path, offset}`; unreadable JSON gives none.
fn read_paths(reads: &str) -> Vec<String> {
    serde_json::from_str::<Vec<serde_json::Value>>(reads)
        .unwrap_or_default()
        .iter()
        .filter_map(|entry| entry.get("path").and_then(serde_json::Value::as_str))
        .filter(|path| !path.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn run_cross(rows: &[CrossAttempt], cfg: &DevtimeRulesConfig) -> Vec<CrossFinding> {
    let mut found = f1(rows, cfg);
    found.extend(f2(rows, cfg));
    found
}

// ---------------------------------------------------------------------------------------------
// F1
// ---------------------------------------------------------------------------------------------

fn f1(rows: &[CrossAttempt], cfg: &DevtimeRulesConfig) -> Vec<CrossFinding> {
    let paths = PathMatcher::new(&cfg.paths);
    let mut cmds: BTreeMap<String, Vec<Item>> = BTreeMap::new();
    let mut reads: BTreeMap<String, Vec<Item>> = BTreeMap::new();
    for row in rows {
        let Some((started_ms, done_ms)) = interval_of(row) else {
            continue;
        };
        if is_shell(row, cfg) {
            if let Some(hash) = row.cmd_hash.as_deref().filter(|hash| !hash.is_empty()) {
                cmds.entry(row.session_id.clone()).or_default().push(Item {
                    key: hash.to_string(),
                    started_ms,
                    done_ms,
                });
            }
        }
        for path in read_paths(&row.reads) {
            if paths.is_external(&path) {
                continue;
            }
            reads.entry(row.session_id.clone()).or_default().push(Item {
                key: path,
                started_ms,
                done_ms,
            });
        }
    }
    // The query's order is not trusted: a stable sort keeps the input order between equal starts.
    for items in cmds.values_mut().chain(reads.values_mut()) {
        items.sort_by_key(|item| item.started_ms);
    }

    let n = (cfg.thresholds.f_sequence_len as usize).max(1);
    let min_sessions = cfg.thresholds.f_min_sessions as usize;
    let mut found = sequences(&cmds, KIND_CMD, n, min_sessions);
    found.extend(sequences(&reads, KIND_READ, n, min_sessions));
    found.sort_by(|a, b| (a.started_ms, &a.anchor).cmp(&(b.started_ms, &b.anchor)));
    found
}

/// The qualifying sequences of one kind: every n-gram counted by distinct session, then adjacent
/// n-grams with an identical session set merged into one chain.
fn sequences(
    per_session: &BTreeMap<String, Vec<Item>>,
    kind: &str,
    n: usize,
    min_sessions: usize,
) -> Vec<CrossFinding> {
    let mut grams: BTreeMap<Vec<String>, BTreeSet<String>> = BTreeMap::new();
    for (session, items) in per_session {
        if items.len() < n {
            continue;
        }
        for window in items.windows(n) {
            let gram: Vec<String> = window.iter().map(|item| item.key.clone()).collect();
            grams.entry(gram).or_default().insert(session.clone());
        }
    }
    grams.retain(|_, sessions| sessions.len() >= min_sessions);

    // Two n-grams are adjacent when the second starts where the first's last n-1 items end. With
    // n = 1 nothing overlaps, so nothing is adjacent and every item stands alone.
    let mut by_prefix: BTreeMap<Vec<String>, Vec<Vec<String>>> = BTreeMap::new();
    if n > 1 {
        for gram in grams.keys() {
            by_prefix
                .entry(gram[..n - 1].to_vec())
                .or_default()
                .push(gram.clone());
        }
    }
    let mut has_predecessor: BTreeSet<Vec<String>> = BTreeSet::new();
    for (gram, sessions) in &grams {
        if let Some(followers) = by_prefix.get(&gram[1..].to_vec()) {
            for follower in followers {
                if follower != gram && grams[follower] == *sessions {
                    has_predecessor.insert(follower.clone());
                }
            }
        }
    }

    // Chains start at n-grams with no predecessor; a ring of n-grams that all have one is picked up
    // by the second pass, so every qualifying n-gram lands in exactly one chain.
    let starts: Vec<&Vec<String>> = grams
        .keys()
        .filter(|gram| !has_predecessor.contains(*gram))
        .chain(grams.keys().filter(|gram| has_predecessor.contains(*gram)))
        .collect();
    let mut visited: BTreeSet<Vec<String>> = BTreeSet::new();
    let mut found = Vec::new();
    for start in starts {
        if !visited.insert(start.clone()) {
            continue;
        }
        let sessions = &grams[start];
        let mut sequence = start.clone();
        let mut tail = start.clone();
        loop {
            let next = by_prefix
                .get(&tail[1..].to_vec())
                .and_then(|followers| {
                    followers
                        .iter()
                        .find(|gram| !visited.contains(*gram) && grams[*gram] == *sessions)
                })
                .cloned();
            let Some(next) = next else {
                break;
            };
            visited.insert(next.clone());
            sequence.push(next[n - 1].clone());
            tail = next;
        }
        found.push(sequence_finding(per_session, kind, n, &sequence, sessions));
    }
    found
}

/// The finding for one chain: where each session first did it, and what the repeats cost.
fn sequence_finding(
    per_session: &BTreeMap<String, Vec<Item>>,
    kind: &str,
    n: usize,
    sequence: &[String],
    sessions: &BTreeSet<String>,
) -> CrossFinding {
    // (first started, last done, session) of the first occurrence in each session. The whole chain is
    // looked for first; a chain that branched may not be contiguous anywhere, and then its first
    // n-gram, which every one of its sessions holds, stands in.
    let mut occurrences: Vec<(i64, i64, &str)> = Vec::new();
    for session in sessions {
        let Some(items) = per_session.get(session) else {
            continue;
        };
        let (at, len) = match first_at(items, sequence) {
            Some(at) => (at, sequence.len()),
            None => match first_at(items, &sequence[..n]) {
                Some(at) => (at, n),
                None => continue,
            },
        };
        let span = &items[at..at + len];
        let done = span.iter().map(|item| item.done_ms).max().unwrap_or(0);
        occurrences.push((span[0].started_ms, done, session.as_str()));
    }
    occurrences.sort();

    let started_ms = occurrences.first().map_or(0, |o| o.0);
    let ended_ms = occurrences.iter().map(|o| o.1).max().unwrap_or(started_ms);
    // The earliest session did the work first; what the later ones spent redoing it is the waste.
    let cost_ms: i64 = occurrences.iter().skip(1).map(|o| (o.1 - o.0).max(0)).sum();
    let ordered: Vec<String> = occurrences.iter().map(|o| o.2.to_string()).collect();
    let latest_session = ordered.last().cloned().unwrap_or_default();
    CrossFinding {
        rule_id: "F1",
        count: ordered.len() as i64,
        sessions: ordered,
        latest_session,
        started_ms,
        ended_ms,
        cost_ms,
        attempt_ids: Vec::new(),
        anchor: sha16(&format!("{kind}{SEP}{}", sequence.join(SEP))),
    }
}

/// The index of the first run of `items` whose keys are exactly `sequence`.
fn first_at(items: &[Item], sequence: &[String]) -> Option<usize> {
    if sequence.is_empty() || items.len() < sequence.len() {
        return None;
    }
    items.windows(sequence.len()).position(|window| {
        window
            .iter()
            .zip(sequence)
            .all(|(item, key)| item.key == *key)
    })
}

// ---------------------------------------------------------------------------------------------
// F2
// ---------------------------------------------------------------------------------------------

/// One failed shell attempt: its start and end in epoch milliseconds.
type Failure = (i64, i64);

fn f2(rows: &[CrossAttempt], cfg: &DevtimeRulesConfig) -> Vec<CrossFinding> {
    // (error_class, cmd_program) -> session -> that session's failures.
    let mut keyed: BTreeMap<(String, String), BTreeMap<String, Vec<Failure>>> = BTreeMap::new();
    for row in rows {
        if !is_shell(row, cfg) || row.outcome != OUTCOME_ERROR {
            continue;
        }
        let (Some(class), Some(program)) = (row.error_class.as_deref(), row.cmd_program.as_deref())
        else {
            continue;
        };
        if class.is_empty() || program.is_empty() {
            continue;
        }
        let Some(failure) = interval_of(row) else {
            continue;
        };
        keyed
            .entry((class.to_string(), program.to_string()))
            .or_default()
            .entry(row.session_id.clone())
            .or_default()
            .push(failure);
    }

    let min_sessions = cfg.thresholds.f_min_sessions as usize;
    let mut found = Vec::new();
    for ((class, program), per_session) in keyed {
        if per_session.len() < min_sessions {
            continue;
        }
        // Sessions by when each first failed this way; the first one is the original, the rest repeat.
        let mut ordered: Vec<(i64, String, Vec<Failure>)> = per_session
            .into_iter()
            .map(|(session, failures)| {
                let first = failures.iter().map(|f| f.0).min().unwrap_or(0);
                (first, session, failures)
            })
            .collect();
        ordered.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));

        let started_ms = ordered.first().map_or(0, |entry| entry.0);
        let ended_ms = ordered
            .iter()
            .flat_map(|entry| entry.2.iter().map(|failure| failure.1))
            .max()
            .unwrap_or(started_ms);
        let cost_ms: i64 = ordered
            .iter()
            .skip(1)
            .flat_map(|entry| entry.2.iter())
            .map(|failure| (failure.1 - failure.0).max(0))
            .sum();
        let sessions: Vec<String> = ordered.iter().map(|entry| entry.1.clone()).collect();
        found.push(CrossFinding {
            rule_id: "F2",
            count: sessions.len() as i64,
            latest_session: sessions.last().cloned().unwrap_or_default(),
            sessions,
            started_ms,
            ended_ms,
            cost_ms,
            attempt_ids: Vec::new(),
            anchor: format!("{class}|{program}"),
        });
    }
    found.sort_by(|a, b| (a.started_ms, &a.anchor).cmp(&(b.started_ms, &b.anchor)));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> DevtimeRulesConfig {
        DevtimeRulesConfig::default()
    }

    /// A timestamp `sec` seconds into `day` of a fixed month.
    fn at(day: u32, sec: u32) -> String {
        format!("2026-10-{day:02}T10:{:02}:{:02}.000Z", sec / 60, sec % 60)
    }

    /// A shell attempt that lasts one second.
    fn shell(session: &str, day: u32, sec: u32) -> CrossAttempt {
        CrossAttempt {
            session_id: session.to_string(),
            started_at: at(day, sec),
            ended_at: Some(at(day, sec + 1)),
            tool_name: "Bash".to_string(),
            outcome: "ok".to_string(),
            reads: "[]".to_string(),
            ..Default::default()
        }
    }

    /// One session's commands, ten seconds apart, starting at second 0 of `day`.
    fn cmds(session: &str, day: u32, hashes: &[&str]) -> Vec<CrossAttempt> {
        hashes
            .iter()
            .enumerate()
            .map(|(i, hash)| CrossAttempt {
                cmd_hash: Some(hash.to_string()),
                cmd_program: Some("cargo".to_string()),
                ..shell(session, day, i as u32 * 10)
            })
            .collect()
    }

    /// One session's reads, ten seconds apart, one path per `Read` attempt.
    fn reads(session: &str, day: u32, files: &[&str]) -> Vec<CrossAttempt> {
        files
            .iter()
            .enumerate()
            .map(|(i, file)| CrossAttempt {
                tool_name: "Read".to_string(),
                reads: format!(r#"[{{"path":"{file}","offset":0}}]"#),
                ..shell(session, day, i as u32 * 10)
            })
            .collect()
    }

    fn failure(session: &str, day: u32, sec: u32, class: &str, program: &str) -> CrossAttempt {
        CrossAttempt {
            cmd_program: Some(program.to_string()),
            cmd_hash: Some(format!("{program}-{session}-{sec}")),
            outcome: "error".to_string(),
            error_class: Some(class.to_string()),
            ..shell(session, day, sec)
        }
    }

    fn cmd_anchor(hashes: &[&str]) -> String {
        sha16(&format!("{KIND_CMD}{SEP}{}", hashes.join(SEP)))
    }

    fn three_sessions(hashes: &[&str]) -> Vec<CrossAttempt> {
        let mut rows = cmds("s1", 1, hashes);
        rows.extend(cmds("s2", 2, hashes));
        rows.extend(cmds("s3", 3, hashes));
        rows
    }

    #[test]
    fn f1_fires_on_three_sessions() {
        let found = run_cross(&three_sessions(&["a", "b", "c"]), &cfg());
        assert_eq!(found.len(), 1, "{found:?}");
        let f = &found[0];
        assert_eq!(f.rule_id, "F1");
        assert_eq!(f.sessions, ["s1", "s2", "s3"]);
        assert_eq!(f.latest_session, "s3");
        assert_eq!(f.count, 3);
        assert_eq!(f.anchor, cmd_anchor(&["a", "b", "c"]));
        // Each occurrence runs from the first command's start to the last one's end: 0s to 21s.
        assert_eq!(f.ended_ms - f.started_ms, 2 * 24 * 3_600_000 + 21_000);
        assert_eq!(f.cost_ms, 2 * 21_000, "the two sessions after the earliest");
        assert!(f.attempt_ids.is_empty());
    }

    #[test]
    fn f1_silent_on_two() {
        let mut rows = cmds("s1", 1, &["a", "b", "c"]);
        rows.extend(cmds("s2", 2, &["a", "b", "c"]));
        assert!(run_cross(&rows, &cfg()).is_empty());
    }

    #[test]
    fn f1_silent_when_three_sessions_share_only_two_hashes() {
        let mut rows = cmds("s1", 1, &["a", "b", "c"]);
        rows.extend(cmds("s2", 2, &["a", "b", "d"]));
        rows.extend(cmds("s3", 3, &["a", "b", "e"]));
        assert!(run_cross(&rows, &cfg()).is_empty());
    }

    #[test]
    fn f1_silent_when_one_session_repeats_it() {
        let rows = cmds("s1", 1, &["a", "b", "c", "a", "b", "c", "a", "b", "c"]);
        assert!(
            run_cross(&rows, &cfg()).is_empty(),
            "distinct sessions only"
        );
    }

    #[test]
    fn f1_merges_adjacent_ngrams() {
        let found = run_cross(&three_sessions(&["a", "b", "c", "d"]), &cfg());
        assert_eq!(
            found.len(),
            1,
            "abc and bcd are one longer sequence: {found:?}"
        );
        assert_eq!(found[0].anchor, cmd_anchor(&["a", "b", "c", "d"]));
        assert_eq!(found[0].cost_ms, 2 * 31_000);
        assert_eq!(found[0].sessions, ["s1", "s2", "s3"]);
    }

    #[test]
    fn f1_does_not_merge_ngrams_with_different_session_sets() {
        let mut rows = cmds("s1", 1, &["a", "b", "c", "d"]);
        rows.extend(cmds("s2", 2, &["a", "b", "c", "d"]));
        rows.extend(cmds("s3", 3, &["a", "b", "c"]));
        rows.extend(cmds("s4", 4, &["b", "c", "d"]));
        let found = run_cross(&rows, &cfg());
        let anchors: BTreeSet<String> = found.iter().map(|f| f.anchor.clone()).collect();
        assert_eq!(found.len(), 2, "{found:?}");
        assert_eq!(
            anchors,
            BTreeSet::from([cmd_anchor(&["a", "b", "c"]), cmd_anchor(&["b", "c", "d"])])
        );
    }

    #[test]
    fn f1_ignores_non_shell_commands_and_missing_hashes() {
        let mut rows = three_sessions(&["a", "b", "c"]);
        for row in &mut rows {
            row.tool_name = "Task".to_string();
        }
        assert!(run_cross(&rows, &cfg()).is_empty());
        let mut rows = three_sessions(&["a", "b", "c"]);
        for row in &mut rows {
            row.cmd_hash = None;
        }
        assert!(run_cross(&rows, &cfg()).is_empty());
    }

    #[test]
    fn f1_read_paths_variant() {
        let mut rows = reads("s1", 1, &["core/a.rs", "core/b.rs", "core/c.rs"]);
        rows.extend(reads("s2", 2, &["core/a.rs", "core/b.rs", "core/c.rs"]));
        rows.extend(reads("s3", 3, &["core/a.rs", "core/b.rs", "core/c.rs"]));
        let found = run_cross(&rows, &cfg());
        assert_eq!(found.len(), 1, "{found:?}");
        let f = &found[0];
        assert_eq!(f.rule_id, "F1");
        assert_eq!(f.sessions, ["s1", "s2", "s3"]);
        let joined = ["core/a.rs", "core/b.rs", "core/c.rs"].join(SEP);
        assert_eq!(f.anchor, sha16(&format!("{KIND_READ}{SEP}{joined}")));
        assert_ne!(
            f.anchor,
            sha16(&format!("{KIND_CMD}{SEP}{joined}")),
            "the kind is part of the anchor"
        );
    }

    #[test]
    fn f1_read_paths_silent_on_two_sessions() {
        let mut rows = reads("s1", 1, &["a.rs", "b.rs", "c.rs"]);
        rows.extend(reads("s2", 2, &["a.rs", "b.rs", "c.rs"]));
        assert!(run_cross(&rows, &cfg()).is_empty());
    }

    #[test]
    fn f1_skips_rows_with_bad_timestamps_and_bad_reads() {
        let mut rows = three_sessions(&["a", "b", "c"]);
        rows[0].started_at = "not a time".to_string();
        assert!(
            run_cross(&rows, &cfg()).is_empty(),
            "s1 lost its first command, so only two sessions hold the run"
        );
        let mut rows = reads("s1", 1, &["a.rs", "b.rs", "c.rs"]);
        rows[0].reads = "not json".to_string();
        let _ = run_cross(&rows, &cfg());
    }

    #[test]
    fn f1_is_deterministic_whatever_the_input_order() {
        let rows = three_sessions(&["a", "b", "c", "d"]);
        let first = format!("{:?}", run_cross(&rows, &cfg()));
        assert_eq!(first, format!("{:?}", run_cross(&rows, &cfg())));
        let mut reversed = rows;
        reversed.reverse();
        assert_eq!(first, format!("{:?}", run_cross(&reversed, &cfg())));
    }

    #[test]
    fn f2_fires_on_three_sessions() {
        let mut rows = vec![failure("s1", 1, 0, "exit_nonzero", "cargo")];
        rows.push(failure("s2", 2, 0, "exit_nonzero", "cargo"));
        rows.push(failure("s2", 2, 30, "exit_nonzero", "cargo"));
        rows.push(failure("s3", 3, 0, "exit_nonzero", "cargo"));
        let found = run_cross(&rows, &cfg());
        assert_eq!(found.len(), 1, "{found:?}");
        let f = &found[0];
        assert_eq!(f.rule_id, "F2");
        assert_eq!(f.anchor, "exit_nonzero|cargo");
        assert_eq!(f.sessions, ["s1", "s2", "s3"]);
        assert_eq!(f.latest_session, "s3");
        assert_eq!(f.count, 3);
        assert_eq!(
            f.cost_ms, 3_000,
            "three one-second failures outside the earliest session"
        );
    }

    #[test]
    fn f2_silent_on_different_programs() {
        let rows = vec![
            failure("s1", 1, 0, "exit_nonzero", "cargo"),
            failure("s2", 2, 0, "exit_nonzero", "npm"),
            failure("s3", 3, 0, "exit_nonzero", "go"),
        ];
        assert!(run_cross(&rows, &cfg()).is_empty());
    }

    #[test]
    fn f2_silent_on_different_classes() {
        let rows = vec![
            failure("s1", 1, 0, "exit_nonzero", "cargo"),
            failure("s2", 2, 0, "tool_error", "cargo"),
            failure("s3", 3, 0, "timeout", "cargo"),
        ];
        assert!(run_cross(&rows, &cfg()).is_empty());
    }

    #[test]
    fn f2_silent_on_two_sessions_and_on_one_session_repeating() {
        let rows = vec![
            failure("s1", 1, 0, "exit_nonzero", "cargo"),
            failure("s2", 2, 0, "exit_nonzero", "cargo"),
        ];
        assert!(run_cross(&rows, &cfg()).is_empty());
        let rows = vec![
            failure("s1", 1, 0, "exit_nonzero", "cargo"),
            failure("s1", 1, 10, "exit_nonzero", "cargo"),
            failure("s1", 1, 20, "exit_nonzero", "cargo"),
        ];
        assert!(
            run_cross(&rows, &cfg()).is_empty(),
            "distinct sessions only"
        );
    }

    #[test]
    fn f2_ignores_successes_non_shell_tools_and_classless_failures() {
        let mut ok = failure("s1", 1, 0, "exit_nonzero", "cargo");
        ok.outcome = "ok".to_string();
        let mut other_tool = failure("s2", 2, 0, "exit_nonzero", "cargo");
        other_tool.tool_name = "Task".to_string();
        let mut classless = failure("s3", 3, 0, "exit_nonzero", "cargo");
        classless.error_class = None;
        assert!(run_cross(&[ok, other_tool, classless], &cfg()).is_empty());
    }

    #[test]
    fn f1_and_f2_can_fire_together_in_a_stable_order() {
        let mut rows = three_sessions(&["a", "b", "c"]);
        rows.push(failure("s1", 1, 100, "exit_nonzero", "cargo"));
        rows.push(failure("s2", 2, 100, "exit_nonzero", "cargo"));
        rows.push(failure("s3", 3, 100, "exit_nonzero", "cargo"));
        let found = run_cross(&rows, &cfg());
        let ids: Vec<&str> = found.iter().map(|f| f.rule_id).collect();
        assert_eq!(ids, ["F1", "F2"]);
        assert_eq!(
            format!("{found:?}"),
            format!("{:?}", run_cross(&rows, &cfg()))
        );
    }
}
