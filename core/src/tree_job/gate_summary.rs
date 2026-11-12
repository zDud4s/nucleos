//! What the owner's inbox shows about a stopped branch: the failing lines of a gate log (taken from
//! the whole log, not its tail) and the reason a node gave for stopping (taken from its report).

use sqlx::SqlitePool;

use super::report::parse_report;

/// The longest digest of failing lines kept on a gate entry.
pub(super) const FAILURES_MAX: usize = 1200;
/// The longest blocked reason appended to a question.
const REASON_MAX: usize = 1200;

/// Whether a log line names a failure: a `FAIL` step, a failed test, a compiler error or a panic.
fn is_failure_line(line: &str) -> bool {
    let line = line.trim();
    line.starts_with("FAIL")
        || line.starts_with("error")
        || line.starts_with("thread '")
        || (line.starts_with("test ") && line.ends_with("FAILED"))
        || line.contains("panicked at")
}

/// The failing lines of the whole `output`, in order, without repeats, at most `FAILURES_MAX` bytes.
/// Empty when the log names no failure.
pub(super) fn failure_digest(output: &str) -> String {
    let mut seen: Vec<&str> = Vec::new();
    let mut digest = String::new();
    for line in output.lines().map(str::trim).filter(|l| is_failure_line(l)) {
        if seen.contains(&line) {
            continue;
        }
        seen.push(line);
        if digest.len() + line.len() + 1 > FAILURES_MAX {
            break;
        }
        if !digest.is_empty() {
            digest.push('\n');
        }
        digest.push_str(line);
    }
    digest
}

/// The findings a fix relaunch or an inbox question shows: the failing lines first, then as much of
/// the log tail as `max` leaves room for.
pub(super) fn findings(failures: &str, log_tail: &str, max: usize) -> String {
    if failures.is_empty() || log_tail.contains(failures) {
        return keep_end(log_tail, max);
    }
    let head = format!("Failing:\n{failures}\n\nLog tail:\n");
    if head.len() >= max {
        return keep_end(&head, max);
    }
    format!("{head}{}", keep_end(log_tail, max - head.len()))
}

fn keep_end(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    text[start..].to_owned()
}

/// The reason a node gave for stopping, from its rendered report: the blocked reason, else the
/// summary, plus the open items. Empty when the report says nothing.
pub(super) fn blocked_reason_of(report_text: &str) -> String {
    let report = parse_report(report_text);
    let mut parts: Vec<String> = Vec::new();
    for (label, text) in [
        ("Blocked reason", &report.blocked_reason),
        ("Summary", &report.summary),
        ("Open items", &report.open_items),
    ] {
        if !text.trim().is_empty() {
            parts.push(format!("{label}: {}", text.trim()));
        }
    }
    let joined = parts.join("\n");
    if joined.chars().count() <= REASON_MAX {
        return joined;
    }
    let cut: String = joined.chars().take(REASON_MAX).collect();
    format!("{cut}…")
}

/// The node's own reason for stopping, as a question suffix (`"\n\n..."`), or `""` when its report
/// cannot be read.
pub(super) async fn reason_suffix(pool: &SqlitePool, node_id: &str) -> String {
    let path: Option<Option<String>> =
        sqlx::query_scalar("SELECT handoff_path FROM work_nodes WHERE id = ?")
            .bind(node_id)
            .fetch_optional(pool)
            .await
            .unwrap_or(None);
    let Some(path) = path.flatten() else {
        return String::new();
    };
    let Ok(text) = tokio::fs::read_to_string(&path).await else {
        return String::new();
    };
    let reason = blocked_reason_of(&text);
    if reason.is_empty() {
        String::new()
    } else {
        format!("\n\nThe node's own report:\n{reason}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_digest_names_failures_that_the_tail_no_longer_holds() {
        let mut log =
            String::from("running tests\ntest tree_job::a::b ... FAILED\nFAIL core: test\n");
        log.push_str(&"filler line\n".repeat(2000));
        log.push_str("FAIL core: test\n");
        let digest = failure_digest(&log);
        assert_eq!(digest, "test tree_job::a::b ... FAILED\nFAIL core: test");
    }

    #[test]
    fn a_log_without_failures_has_an_empty_digest() {
        assert_eq!(failure_digest("all green\nok\n"), "");
    }

    #[test]
    fn compiler_errors_and_panics_count() {
        let log = "error[E0425]: cannot find value `x`\nthread 'main' panicked at src/a.rs:1\n";
        assert_eq!(failure_digest(log).lines().count(), 2);
    }

    #[test]
    fn findings_put_the_failures_before_the_tail_within_the_cap() {
        let out = findings("FAIL core: test", &"x".repeat(500), 200);
        assert!(out.starts_with("Failing:\nFAIL core: test"));
        assert!(out.len() <= 200);
        assert!(out.ends_with('x'));
    }

    #[test]
    fn findings_do_not_repeat_failures_the_tail_already_shows() {
        assert_eq!(findings("FAIL a", "log\nFAIL a\n", 100), "log\nFAIL a\n");
        assert_eq!(findings("", "tail", 100), "tail");
    }

    #[test]
    fn the_blocked_reason_comes_from_the_report() {
        let text = "# Node Handoff\n\nNode ID: n\nRoot ID: r\nStatus: blocked\nRole: leaf\nOutcome: blocked\nContract used: none\n\n## Summary\nCould not reach the disk.\n\n## Open items / next step\nFree space, then retry.\n\n## Blocked reason\nDisk is full.\n";
        let reason = blocked_reason_of(text);
        assert!(reason.contains("Blocked reason: Disk is full."));
        assert!(reason.contains("Summary: Could not reach the disk."));
        assert!(reason.contains("Open items: Free space, then retry."));
    }

    #[test]
    fn an_empty_report_gives_no_reason() {
        assert_eq!(blocked_reason_of(""), "");
    }
}
