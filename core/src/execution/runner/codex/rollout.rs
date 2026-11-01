//! The Codex CLI's context occupancy, read from its rollout file.
//!
//! The `--json` stream has no `token_count` event, and its `turn.completed` `usage.input_tokens`
//! is the SESSION's running total, not what the window holds. Only the rollout
//! (`<codex home>/sessions/YYYY/MM/DD/rollout-<ts>-<thread_id>.jsonl`) records the window: each
//! `event_msg` of type `token_count` carries `info.last_token_usage.input_tokens`, the input of the
//! latest model round.

use std::path::{Path, PathBuf};

/// `input_tokens` of the last round, from the LAST `token_count` event of a rollout's text.
///
/// Events whose `info` is null, lines that are not JSON and other event types are skipped. `None`
/// when no event carries a figure: unknown is not zero.
pub fn rollout_last_input_tokens(rollout: &str) -> Option<i64> {
    rollout.lines().rev().find_map(|line| {
        let value = serde_json::from_str::<serde_json::Value>(line.trim()).ok()?;
        if value.get("type")?.as_str()? != "event_msg" {
            return None;
        }
        let payload = value.get("payload")?;
        if payload.get("type")?.as_str()? != "token_count" {
            return None;
        }
        payload
            .get("info")?
            .get("last_token_usage")?
            .get("input_tokens")?
            .as_i64()
    })
}

/// The rollout file of `thread_id` under `codex_home/sessions/YYYY/MM/DD/`, or `None`.
///
/// The date directories are scanned rather than computed: a resumed turn appends to the file of the
/// day the thread began, not of today. An id that is empty or carries a path separator never
/// matches, so it cannot steer the scan outside the sessions tree.
pub fn find_codex_rollout(codex_home: &Path, thread_id: &str) -> Option<PathBuf> {
    if thread_id.is_empty() || thread_id.contains(['/', '\\']) {
        return None;
    }
    let suffix = format!("-{thread_id}.jsonl");
    let subdirs = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|entry| entry.file_type().is_ok_and(|t| t.is_dir()))
            .map(|entry| entry.path())
            .collect()
    };
    for year in subdirs(&codex_home.join("sessions")) {
        for month in subdirs(&year) {
            for day in subdirs(&month) {
                let found = std::fs::read_dir(&day)
                    .into_iter()
                    .flatten()
                    .flatten()
                    .find(|entry| entry.file_name().to_string_lossy().ends_with(&suffix));
                if let Some(entry) = found {
                    return Some(entry.path());
                }
            }
        }
    }
    None
}

/// The Codex home: `$CODEX_HOME` when non-blank, else `<home>/.codex`. Pure, so the order is
/// testable (same shape as `codex_cache_path` in `model_catalog`).
pub fn codex_home_dir(codex_home: Option<String>, home: Option<String>) -> Option<PathBuf> {
    match codex_home.filter(|h| !h.trim().is_empty()) {
        Some(h) => Some(PathBuf::from(h)),
        None => Some(PathBuf::from(home.filter(|h| !h.trim().is_empty())?).join(".codex")),
    }
}

/// Context occupied by `thread_id`, read from its rollout. `None` without a readable rollout or
/// without a `token_count` event.
pub fn codex_rollout_fill(codex_home: &Path, thread_id: &str) -> Option<i64> {
    let path = find_codex_rollout(codex_home, thread_id)?;
    rollout_last_input_tokens(&std::fs::read_to_string(path).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `token_count` line of the real rollout shape, with the rate limits trimmed away.
    fn token_count(total_input: i64, last_input: i64) -> String {
        format!(
            r#"{{"timestamp":"2026-10-30T23:13:36.661Z","ordinal":22,"type":"event_msg","payload":{{"type":"token_count","info":{{"total_token_usage":{{"input_tokens":{total_input},"cached_input_tokens":0,"output_tokens":9,"total_tokens":{total_input}}},"last_token_usage":{{"input_tokens":{last_input},"cached_input_tokens":0,"output_tokens":9,"total_tokens":{last_input}}},"model_context_window":475000}},"rate_limits":null}}}}"#
        )
    }

    /// A `token_count` the CLI emits before the first round: `info` is null.
    const TOKEN_COUNT_NO_INFO: &str = r#"{"timestamp":"2026-10-30T23:13:30.000Z","ordinal":3,"type":"event_msg","payload":{"type":"token_count","info":null,"rate_limits":null}}"#;

    const OTHER_EVENT: &str = r#"{"timestamp":"2026-10-30T23:13:31.000Z","ordinal":4,"type":"response_item","payload":{"type":"message","role":"assistant"}}"#;

    #[test]
    fn the_last_token_count_wins_and_its_round_input_is_returned_not_the_session_total() {
        let rollout = [
            token_count(14_942, 14_942),
            OTHER_EVENT.to_string(),
            token_count(30_401, 15_459),
            token_count(63_325, 16_979),
        ]
        .join("\n");
        assert_eq!(rollout_last_input_tokens(&rollout), Some(16_979));
    }

    #[test]
    fn a_null_info_and_malformed_lines_are_skipped_around_a_real_figure() {
        let rollout = [
            TOKEN_COUNT_NO_INFO.to_string(),
            "not json at all".to_string(),
            token_count(30_401, 15_459),
            r#"{"type":"event_msg","payload":{"type":"token_count""#.to_string(),
            TOKEN_COUNT_NO_INFO.to_string(),
            String::new(),
        ]
        .join("\n");
        // The trailing null-info event must not erase the earlier figure.
        assert_eq!(rollout_last_input_tokens(&rollout), Some(15_459));
    }

    #[test]
    fn a_rollout_without_any_usable_token_count_has_no_figure() {
        assert_eq!(rollout_last_input_tokens(""), None);
        assert_eq!(rollout_last_input_tokens(OTHER_EVENT), None);
        let only_null = [TOKEN_COUNT_NO_INFO, "garbage"].join("\n");
        assert_eq!(rollout_last_input_tokens(&only_null), None);
    }

    fn write_rollout(home: &Path, day: &str, thread_id: &str, body: &str) -> PathBuf {
        let dir = home.join("sessions").join(day);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("rollout-2026-10-30T23-13-31-{thread_id}.jsonl"));
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn the_rollout_of_a_thread_is_found_by_id_under_the_dated_sessions_tree() {
        let home = tempfile::tempdir().unwrap();
        let wanted = write_rollout(home.path(), "2026/10/30", "thread-aaa", "");
        write_rollout(home.path(), "2026/10/29", "thread-bbb", "");
        assert_eq!(find_codex_rollout(home.path(), "thread-aaa"), Some(wanted));
    }

    #[test]
    fn an_unknown_thread_or_a_home_without_sessions_has_no_rollout() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(find_codex_rollout(home.path(), "thread-aaa"), None);
        write_rollout(home.path(), "2026/10/30", "thread-bbb", "");
        assert_eq!(find_codex_rollout(home.path(), "thread-aaa"), None);
    }

    #[test]
    fn the_fill_of_a_thread_is_read_from_its_own_rollout() {
        let home = tempfile::tempdir().unwrap();
        write_rollout(
            home.path(),
            "2026/10/30",
            "thread-aaa",
            &[token_count(63_325, 16_979)].join("\n"),
        );
        write_rollout(
            home.path(),
            "2026/10/30",
            "thread-bbb",
            &[token_count(500, 400)].join("\n"),
        );
        assert_eq!(codex_rollout_fill(home.path(), "thread-aaa"), Some(16_979));
        assert_eq!(codex_rollout_fill(home.path(), "thread-zzz"), None);
    }

    #[test]
    fn the_codex_home_is_codex_home_when_set_else_dot_codex_under_the_user_home() {
        assert_eq!(
            codex_home_dir(Some("/x/codex".into()), Some("/h".into())),
            Some(PathBuf::from("/x/codex"))
        );
        assert_eq!(
            codex_home_dir(Some("  ".into()), Some("/h".into())),
            Some(Path::new("/h").join(".codex"))
        );
        assert_eq!(
            codex_home_dir(None, Some("/h".into())),
            Some(Path::new("/h").join(".codex"))
        );
        assert_eq!(codex_home_dir(None, None), None);
    }
}
