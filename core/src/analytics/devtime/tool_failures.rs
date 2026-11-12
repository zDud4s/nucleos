//! Metric 7 of the devtime overview (spec 2026-10-17-analytics-devtime §3, §6.1 and §8): the
//! current window's tool attempts folded into a failure rate, by error class and by tool/program.
//!
//! No SQL: a pure fold over `store::attempt_failure_rows`. The denominator is the attempts that
//! completed (`ok`, `error`, `launched`); `interrupted` (the owner stopped it, metric 5 already
//! counts it) and `unknown` (no result seen yet) are reported beside it and are in no denominator.
//! Programs are redacted here, at display (§8 "program name, not arguments"): the parser keeps a
//! second word, which for an interpreter is a script name or path. Stored data is untouched.

use std::collections::BTreeMap;

use serde::Serialize;

use super::{OTHER_KEY, cap_list, ratio};
use crate::devtime::parse::ERROR_CLASSES;
use crate::devtime::store::AttemptFailureRow;

/// The classes of a `run_in_background` call whose task ended badly after its launch.
pub const BACKGROUND_CLASSES: [&str; 2] = ["background_failed", "background_killed"];
/// The class of an `error` whose stored class is NULL or outside the vocabulary.
const FALLBACK_CLASS: &str = "tool_error";
/// The one parser class that is not a failure: the owner stopped the call.
const INTERRUPTED: &str = "interrupted";
const PROGRAM_MAX_CHARS: usize = 48;
const TOOL_MAX_CHARS: usize = 64;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ToolFailures {
    /// Attempts that completed: outcome `ok`, `error` or `launched`.
    pub attempts: i64,
    pub failures: i64,
    /// `failures / attempts`; `None` without attempts.
    pub failure_rate: Option<f64>,
    pub interrupted: i64,
    /// `unknown` (no result seen yet) or any other outcome token.
    pub unresolved: i64,
    pub by_error_class: Vec<ErrorClassFailures>,
    pub by_tool: Vec<ToolFailureRow>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ErrorClassFailures {
    pub error_class: String,
    pub failures: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ToolFailureRow {
    pub tool_name: String,
    /// Redacted by [`program_label`]: at most the program and one subcommand word.
    pub cmd_program: Option<String>,
    pub attempts: i64,
    pub failures: i64,
    pub failure_rate: Option<f64>,
    /// The class with most failures, ties by token asc; `None` without failures.
    pub top_error_class: Option<String>,
}

/// Programs whose second word is a script (or inline code), never a subcommand: their label is
/// the program alone. Compared case-insensitively, after any `.exe` suffix is dropped.
const SCRIPT_RUNNERS: [&str; 16] = [
    "python",
    "python3",
    "py",
    "node",
    "deno",
    "bun",
    "ruby",
    "perl",
    "php",
    "bash",
    "sh",
    "zsh",
    "pwsh",
    "powershell",
    "cmd",
    "rscript",
];

/// Whether `name` (a basename) runs a script: one of [`SCRIPT_RUNNERS`], or a versioned python
/// such as `python3.12`.
fn is_script_runner(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let bare = lower.strip_suffix(".exe").unwrap_or(&lower);
    SCRIPT_RUNNERS.contains(&bare) || bare.starts_with("python")
}

/// Whether `word` looks like a subcommand (`test`, `status`, `run`, `build-std`): ASCII letters,
/// digits, `-` and `_` only, not a flag and not a bare number. Anything with a `.`, `=`, path
/// separator, quote or other punctuation can be a file, an assignment or a value, so it is not.
fn is_subcommand(word: &str) -> bool {
    !word.starts_with('-')
        && word.chars().any(|c| c.is_ascii_alphabetic())
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// A stored program reduced to what §8 allows ("program name, not the arguments"): the first
/// word's basename, plus the second word only when it looks like a subcommand and the program is
/// not a script runner; at most 48 chars. `None` if empty.
pub(crate) fn program_label(raw: &str) -> Option<String> {
    let mut words = raw.split_whitespace();
    let first = words.next()?;
    let name = first
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(first)
        .trim_matches(['"', '\'']);
    if name.is_empty() {
        return None;
    }
    let mut label = name.to_owned();
    if let Some(second) = words.next()
        && !is_script_runner(name)
        && is_subcommand(second)
    {
        label.push(' ');
        label.push_str(second);
    }
    Some(truncate(&label, PROGRAM_MAX_CHARS))
}

fn truncate(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

/// The failure class of an `error`, or of a `launched` whose background task failed: a closed
/// vocabulary token, anything else `tool_error`.
fn failure_class(stored: Option<&str>) -> &'static str {
    ERROR_CLASSES
        .iter()
        .filter(|class| **class != INTERRUPTED)
        .chain(BACKGROUND_CLASSES.iter())
        .find(|class| Some(**class) == stored)
        .copied()
        .unwrap_or(FALLBACK_CLASS)
}

#[derive(Default)]
struct Tally {
    attempts: i64,
    failures: i64,
    classes: BTreeMap<&'static str, i64>,
}

impl Tally {
    fn top_class(&self) -> Option<String> {
        let mut top: Option<(&str, i64)> = None;
        // Ascending token order, so a strictly greater count is the only thing that replaces.
        for (class, failures) in &self.classes {
            if *failures > 0 && top.is_none_or(|(_, best)| *failures > best) {
                top = Some((*class, *failures));
            }
        }
        top.map(|(class, _)| class.to_owned())
    }
}

/// Folds the window's attempt rows into the pinned `tool_failures` block.
pub(crate) fn block(rows: &[AttemptFailureRow]) -> ToolFailures {
    let mut out = ToolFailures::default();
    let mut by_class: BTreeMap<&'static str, i64> = BTreeMap::new();
    let mut by_tool: BTreeMap<(String, Option<String>), Tally> = BTreeMap::new();
    for row in rows {
        let class = match row.outcome.as_str() {
            "ok" => None,
            "error" => Some(failure_class(row.failure_class.as_deref())),
            "launched" => row
                .failure_class
                .as_deref()
                .filter(|class| BACKGROUND_CLASSES.contains(class))
                .map(|class| failure_class(Some(class))),
            INTERRUPTED => {
                out.interrupted += row.attempts;
                continue;
            }
            _ => {
                out.unresolved += row.attempts;
                continue;
            }
        };
        out.attempts += row.attempts;
        let key = (
            truncate(&row.tool_name, TOOL_MAX_CHARS),
            row.cmd_program.as_deref().and_then(program_label),
        );
        let tally = by_tool.entry(key).or_default();
        tally.attempts += row.attempts;
        if let Some(class) = class {
            out.failures += row.attempts;
            *by_class.entry(class).or_default() += row.attempts;
            tally.failures += row.attempts;
            *tally.classes.entry(class).or_default() += row.attempts;
        }
    }
    out.failure_rate = ratio(out.failures, out.attempts);

    let mut classes: Vec<ErrorClassFailures> = by_class
        .into_iter()
        .filter(|(_, failures)| *failures > 0)
        .map(|(class, failures)| ErrorClassFailures {
            error_class: class.to_owned(),
            failures,
        })
        .collect();
    // Stable over the ascending map, so ties stay in token order.
    classes.sort_by_key(|c| std::cmp::Reverse(c.failures));
    out.by_error_class = classes;

    let mut tools: Vec<ToolFailureRow> = by_tool
        .into_iter()
        .map(|((tool_name, cmd_program), tally)| ToolFailureRow {
            failure_rate: ratio(tally.failures, tally.attempts),
            top_error_class: tally.top_class(),
            tool_name,
            cmd_program,
            attempts: tally.attempts,
            failures: tally.failures,
        })
        .collect();
    tools.sort_by(|a, b| {
        b.failures
            .cmp(&a.failures)
            .then(b.attempts.cmp(&a.attempts))
            .then_with(|| a.tool_name.cmp(&b.tool_name))
            .then_with(|| a.cmd_program.cmp(&b.cmd_program))
    });
    out.by_tool = cap_list(tools, other_row);
    out
}

/// The rows past the cap, summed into one `__other__` row with its rate recomputed.
fn other_row(rest: &[ToolFailureRow]) -> ToolFailureRow {
    let attempts = rest.iter().map(|row| row.attempts).sum();
    let failures = rest.iter().map(|row| row.failures).sum();
    ToolFailureRow {
        tool_name: OTHER_KEY.to_owned(),
        cmd_program: None,
        attempts,
        failures,
        failure_rate: ratio(failures, attempts),
        top_error_class: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analytics::{Scope, Window};
    use crate::config::ModelsConfig;
    use crate::devtime::store::attempt_fixture::{Attempt, attempt, insert_attempt};
    use crate::devtime::store::fixture::session;
    use chrono::{DateTime, Utc};

    fn row(
        tool: &str,
        program: Option<&str>,
        outcome: &str,
        class: Option<&str>,
        attempts: i64,
    ) -> AttemptFailureRow {
        AttemptFailureRow {
            tool_name: tool.to_owned(),
            cmd_program: program.map(str::to_owned),
            outcome: outcome.to_owned(),
            failure_class: class.map(str::to_owned),
            attempts,
        }
    }

    fn class(error_class: &str, failures: i64) -> ErrorClassFailures {
        ErrorClassFailures {
            error_class: error_class.to_owned(),
            failures,
        }
    }

    #[test]
    fn failure_rate_is_failures_over_completed_attempts() {
        let block = block(&[
            row("Read", None, "ok", None, 6),
            row("Bash", Some("cargo test"), "error", Some("exit_nonzero"), 2),
            row("Bash", None, "launched", None, 1),
            row("Bash", None, "launched", Some("background_failed"), 1),
            row("Bash", None, "interrupted", Some("interrupted"), 3),
            row("Edit", None, "unknown", None, 2),
        ]);
        assert_eq!(block.attempts, 10);
        assert_eq!(block.failures, 3);
        assert_eq!(block.failure_rate, Some(0.3));
        assert_eq!(block.interrupted, 3);
        assert_eq!(block.unresolved, 2);
        assert_eq!(
            block.by_error_class,
            vec![class("exit_nonzero", 2), class("background_failed", 1)]
        );
    }

    #[test]
    fn failure_rate_is_null_without_completed_attempts() {
        let only_open = block(&[
            row("Bash", None, "interrupted", Some("interrupted"), 2),
            row("Bash", None, "unknown", None, 1),
        ]);
        assert_eq!(only_open.attempts, 0);
        assert_eq!(only_open.failures, 0);
        assert_eq!(only_open.failure_rate, None);
        assert_eq!((only_open.interrupted, only_open.unresolved), (2, 1));
        assert!(only_open.by_error_class.is_empty());
        assert!(only_open.by_tool.is_empty());

        assert_eq!(block(&[]), ToolFailures::default());
    }

    #[test]
    fn by_error_class_is_sorted_and_omits_zero() {
        let block = block(&[
            row("Edit", None, "error", Some("edit_not_found"), 2),
            row("Bash", None, "error", Some("timeout"), 3),
            row("Bash", None, "error", Some("exit_75"), 2),
            row("Bash", None, "ok", None, 9),
        ]);
        assert_eq!(
            block.by_error_class,
            vec![
                class("timeout", 3),
                class("edit_not_found", 2),
                class("exit_75", 2)
            ]
        );
    }

    #[test]
    fn unknown_or_missing_class_on_an_error_counts_as_tool_error() {
        let block = block(&[
            row("Bash", None, "error", None, 1),
            row("Bash", None, "error", Some("made_up"), 2),
            row("Bash", None, "error", Some("interrupted"), 1),
        ]);
        assert_eq!(block.failures, 4);
        assert_eq!(block.by_error_class, vec![class("tool_error", 4)]);
        assert_eq!(
            block.by_tool[0].top_error_class.as_deref(),
            Some("tool_error")
        );
    }

    #[test]
    fn program_label_never_keeps_a_path_or_more_than_two_words() {
        let label = program_label;
        assert_eq!(label("cargo test").as_deref(), Some("cargo test"));
        assert_eq!(
            label("python3 C:/Users/x/run.py").as_deref(),
            Some("python3")
        );
        assert_eq!(label("node \"C:\\a b\\x.js\"").as_deref(), Some("node"));
        assert_eq!(label("git ~/x").as_deref(), Some("git"));
        assert_eq!(label("a b c").as_deref(), Some("a b"));
        assert_eq!(label("cargo --version").as_deref(), Some("cargo"));
        assert_eq!(label("/usr/bin/git status").as_deref(), Some("git status"));
        let long = "é".repeat(200);
        let cut = label(&long).unwrap();
        assert!(cut.chars().count() <= 48 && long.starts_with(&cut), "{cut}");
        assert_eq!(label(""), None);
        assert_eq!(label("   "), None);
    }

    #[test]
    fn program_label_drops_the_script_of_an_interpreter() {
        let label = program_label;
        assert_eq!(label("python3 run.py").as_deref(), Some("python3"));
        assert_eq!(label("bash deploy_x.sh").as_deref(), Some("bash"));
        assert_eq!(label("node script.js").as_deref(), Some("node"));
        // Even a script name that would pass as a subcommand: the runner says it is a script.
        assert_eq!(label("bash deploy").as_deref(), Some("bash"));
        assert_eq!(label("python.exe manage").as_deref(), Some("python.exe"));
        assert_eq!(
            label("PowerShell.EXE deploy").as_deref(),
            Some("PowerShell.EXE")
        );
        assert_eq!(label("python3.12 tool").as_deref(), Some("python3.12"));
        assert_eq!(label("Rscript analyse").as_deref(), Some("Rscript"));
        assert_eq!(label("C:/bin/pwsh.exe x").as_deref(), Some("pwsh.exe"));
    }

    #[test]
    fn program_label_keeps_only_a_subcommand_shaped_second_word() {
        let label = program_label;
        assert_eq!(label("git status").as_deref(), Some("git status"));
        assert_eq!(label("cargo test").as_deref(), Some("cargo test"));
        assert_eq!(label("npm run").as_deref(), Some("npm run"));
        assert_eq!(label("cargo build-std").as_deref(), Some("cargo build-std"));
        assert_eq!(label("make deploy_x.sh").as_deref(), Some("make"));
        assert_eq!(label("env FOO=bar").as_deref(), Some("env"));
        assert_eq!(label("sleep 30").as_deref(), Some("sleep"));
        assert_eq!(label("kill 1234").as_deref(), Some("kill"));
        assert_eq!(label("tool a,b").as_deref(), Some("tool"));
    }

    #[test]
    fn by_tool_merges_rows_that_redact_to_the_same_program() {
        let block = block(&[
            row(
                "Bash",
                Some("python3 C:/a/x.py"),
                "error",
                Some("exit_nonzero"),
                1,
            ),
            row("Bash", Some("python3 D:/b/y.py"), "ok", None, 3),
        ]);
        assert_eq!(
            block.by_tool,
            vec![ToolFailureRow {
                tool_name: "Bash".into(),
                cmd_program: Some("python3".into()),
                attempts: 4,
                failures: 1,
                failure_rate: Some(0.25),
                top_error_class: Some("exit_nonzero".into()),
            }]
        );
    }

    #[test]
    fn by_tool_caps_at_twenty_and_folds_the_rest_into_other() {
        // Tool `t00` fails most and `t24` least, so the five folded are `t20..=t24`.
        let rows: Vec<AttemptFailureRow> = (0..25)
            .flat_map(|n: i64| {
                let tool = format!("t{n:02}");
                [
                    row(&tool, None, "error", Some("timeout"), 30 - n),
                    row(&tool, None, "ok", None, 10),
                ]
            })
            .collect();
        let block = block(&rows);
        assert_eq!(block.by_tool.len(), 21);
        assert_eq!(block.by_tool[0].tool_name, "t00");
        assert_eq!(block.by_tool[19].tool_name, "t19");
        let other = &block.by_tool[20];
        // Failures 10 + 9 + 8 + 7 + 6 = 40 over 40 + 50 attempts.
        assert_eq!(
            other,
            &ToolFailureRow {
                tool_name: OTHER_KEY.into(),
                cmd_program: None,
                attempts: 90,
                failures: 40,
                failure_rate: Some(40.0 / 90.0),
                top_error_class: None,
            }
        );
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
            &crate::config::DevtimeRulesConfig::default(),
            scope,
            Window::new(7, now).unwrap(),
            now,
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn overview_counts_the_current_window_only_and_scopes_by_project() {
        let pool = crate::testdb::fresh_pool().await;
        let start = "2026-10-01T00:00:00.000Z";
        session(&pool, "a1", "alpha", start, None).await;
        session(&pool, "b1", "beta", start, None).await;
        fn error<'a>(session: &'a str, at: &'a str) -> Attempt<'a> {
            Attempt {
                outcome: "error",
                error_class: Some("exit_nonzero"),
                ..attempt(session, "Bash", at)
            }
        }
        insert_attempt(&pool, error("a1", "2026-10-12T10:00:00.000Z")).await;
        insert_attempt(&pool, attempt("b1", "Read", "2026-10-13T10:00:00.000Z")).await;
        // The previous window, and after now: neither counts.
        insert_attempt(&pool, error("a1", "2026-10-08T10:00:00.000Z")).await;
        insert_attempt(&pool, error("a1", "2026-10-17T13:00:00.000Z")).await;

        let global = overview(&pool, Scope::Global).await.tool_failures;
        assert_eq!((global.attempts, global.failures), (2, 1));
        assert_eq!(global.failure_rate, Some(0.5));

        let alpha = overview(&pool, Scope::Project("alpha")).await.tool_failures;
        assert_eq!((alpha.attempts, alpha.failures), (1, 1));
        assert_eq!(alpha.by_tool.len(), 1);
        assert_eq!(alpha.by_tool[0].tool_name, "Bash");
    }

    #[tokio::test]
    async fn tool_failures_json_carries_no_session_worktree_or_argument() {
        let pool = crate::testdb::fresh_pool().await;
        session(
            &pool,
            "sess-secret-77",
            "alpha",
            "2026-10-12T09:00:00.000Z",
            None,
        )
        .await;
        sqlx::query("UPDATE devtime_sessions SET worktree = 'C:/wt/secret'")
            .execute(&pool)
            .await
            .unwrap();
        let failed = Attempt {
            cmd_program: Some("python3 C:/Users/secret/run.py"),
            outcome: "error",
            error_class: Some("exit_nonzero"),
            ..attempt("sess-secret-77", "Bash", "2026-10-12T10:00:00.000Z")
        };
        insert_attempt(&pool, failed).await;

        let report = serde_json::to_value(overview(&pool, Scope::Global).await).unwrap();
        let block = &report["tool_failures"];
        assert!(block.is_object(), "{report}");
        assert_eq!(block["failures"], 1, "{block}");
        let text = block.to_string();
        for needle in ["sess-secret-77", "C:/", "secret", "/wt"] {
            assert!(!text.contains(needle), "{needle} in {text}");
        }
        // Every reserved follow-up key is present since follow-up C (metric 10).
        for key in ["turn_classes", "unexplained", "agents"] {
            assert!(report[key].is_object(), "{key} in {report}");
        }
    }
}
