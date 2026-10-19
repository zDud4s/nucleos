//! Filtering what a test, check or build command prints before a model reads it: a streaming view
//! that keeps the tail and the failure windows, the Bash and PowerShell wrappers that route a
//! command through it, the Codex instruction text, the per-run log directory and its pruning.

pub mod run;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::SqlitePool;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

/// Lines kept from a run that exited 0.
pub const GREEN_TAIL: usize = 10;
/// Lines kept from the end of a run that failed, next to its failure windows.
pub const RED_TAIL: usize = 20;
/// Lines kept from a failed run in which nothing looked like a failure.
pub const NO_MATCH_TAIL: usize = 60;
/// Lines kept after a failure line.
pub const AFTER_MATCH: usize = 5;
/// The most failure-window lines one run keeps, so memory stays bounded however loud it is.
pub const MAX_MATCH_LINES: usize = 100;
/// A line longer than this is cut and marked.
pub const LINE_MAX_CHARS: usize = 500;
/// The raw log of one run keeps at most this much, and keeps its TAIL (decision 2026-08-20: a
/// capped stream says which half it kept).
pub const LOG_CAP_BYTES: u64 = 64 * 1024 * 1024;
/// A command longer than this is left alone rather than wrapped.
pub const MAX_WRAPPED_BYTES: usize = 128 * 1024;
/// The Windows command line is 32767 characters; the encoded command stays well under it.
pub const MAX_ENCODED_CHARS: usize = 30_000;
/// The line the Bash wrapper prints after the command, carrying its exit status.
pub const EXIT_SENTINEL: &str = "\u{1e}NUCLEOS-EXIT:";
/// The flag that routes a process into the filter runtime. It is also how a wrapped command is
/// recognised, so a command is never wrapped twice.
pub const FLAG: &str = "--filter-run";
/// The directory under the data directory that holds one subdirectory of logs per run.
pub const RUN_OUTPUT_DIR: &str = "run-output";

/// What the PowerShell wrapper appends to the command, so the exit status reaches the filter.
pub const PS_POSTAMBLE: &str = "$__nucleosOk = $?\nif ($global:LASTEXITCODE) { exit $global:LASTEXITCODE }\nif (-not $__nucleosOk) { exit 1 }\nexit 0";

static FAILURE_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"(?i)\b(fail(ed|ure|ures|s)?|error(s)?|panic(ked|s)?|assert(ion)?|expected|traceback|exception)\b",
    )
    .expect("the failure pattern is valid")
});

/// A streaming view of a command's output: it sees every line once and keeps only what `render`
/// can need, so a build that prints a million lines costs the same memory as one that prints ten.
#[derive(Debug, Default)]
pub struct View {
    total: usize,
    ring: VecDeque<(usize, String)>,
    windows: Vec<(usize, String)>,
    after_left: usize,
    matched: bool,
}

impl View {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push_line(&mut self, line: &str) {
        let index = self.total;
        self.total += 1;
        let line = clean_line(line);

        if FAILURE_RE.is_match(&line) {
            self.matched = true;
            self.after_left = AFTER_MATCH;
            if self.windows.len() < MAX_MATCH_LINES {
                self.windows.push((index, line.clone()));
            }
        } else if self.after_left > 0 {
            self.after_left -= 1;
            if self.windows.len() < MAX_MATCH_LINES {
                self.windows.push((index, line.clone()));
            }
        }

        if self.ring.len() == NO_MATCH_TAIL {
            self.ring.pop_front();
        }
        self.ring.push_back((index, line));
    }

    /// The number of lines seen so far.
    pub fn total_lines(&self) -> usize {
        self.total
    }

    pub fn render<'a>(&'a self, exit_code: i32, footer: &str) -> String {
        let mut kept: BTreeMap<usize, &'a str> = BTreeMap::new();
        let tail = |count: usize, kept: &mut BTreeMap<usize, &'a str>| {
            let skip = self.ring.len().saturating_sub(count);
            for (index, line) in self.ring.iter().skip(skip) {
                kept.insert(*index, line.as_str());
            }
        };
        if exit_code == 0 {
            tail(GREEN_TAIL, &mut kept);
        } else if self.matched {
            for (index, line) in &self.windows {
                kept.insert(*index, line.as_str());
            }
            tail(RED_TAIL, &mut kept);
        } else {
            tail(NO_MATCH_TAIL, &mut kept);
        }

        let mut out = String::new();
        let mut next = 0usize;
        for (index, line) in kept {
            if index > next {
                out.push_str(&format!("… {} lines omitted …\n", index - next));
            }
            out.push_str(line);
            out.push('\n');
            next = index + 1;
        }
        if self.total > next {
            out.push_str(&format!("… {} lines omitted …\n", self.total - next));
        }
        out.push_str(footer);
        out
    }
}

/// A line as it is kept: no trailing carriage return, and cut at `LINE_MAX_CHARS` on a char
/// boundary with an ellipsis so the cut is visible.
fn clean_line(line: &str) -> String {
    let line = line.strip_suffix('\r').unwrap_or(line);
    match line.char_indices().nth(LINE_MAX_CHARS) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_string(),
    }
}

/// PURE: the filtered view of a whole output at once.
pub fn apply(output: &str, exit_code: i32, footer: &str) -> String {
    let mut view = View::new();
    for line in output.lines() {
        view.push_line(line);
    }
    view.render(exit_code, footer)
}

/// PURE: the last line of the view, which says where the full output is.
pub fn footer(exit_code: i32, total_lines: usize, log: Result<&[PathBuf], &str>) -> String {
    match log {
        Ok(paths) => {
            let shown: Vec<String> = paths
                .iter()
                .map(|path| path.display().to_string())
                .collect();
            format!(
                "[nucleos] exit {exit_code} · {total_lines} lines · full log: {}",
                shown.join(", ")
            )
        }
        Err(why) => format!("[nucleos] exit {exit_code} · full log unavailable: {why}"),
    }
}

/// A path as the wrapper writes it: forward slashes, which both shells accept on Windows.
fn forward(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn quote_bash(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

fn quote_powershell(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// PURE: `command` run in a subshell with stderr merged, its status printed as a sentinel line,
/// all of it piped through the filter runtime. `None` when the command is too big to wrap.
pub fn wrap_bash(command: &str, exe: &Path, log_dir: &Path) -> Option<String> {
    if command.len() > MAX_WRAPPED_BYTES {
        return None;
    }
    Some(format!(
        "{{ ( {command}\n) 2>&1; printf '\\n\\036NUCLEOS-EXIT:%d\\n' \"$?\"; }} | {} {FLAG} --log {}",
        quote_bash(&forward(exe)),
        quote_bash(&forward(log_dir)),
    ))
}

/// PURE: `command` handed to the filter runtime as an encoded PowerShell command, which runs it
/// in a child of the calling shell. `None` when the encoded command would not fit a command line.
pub fn wrap_powershell(command: &str, exe: &Path, log_dir: &Path) -> Option<String> {
    if command.len() > MAX_WRAPPED_BYTES {
        return None;
    }
    let script = format!("{command}\n{PS_POSTAMBLE}");
    let bytes: Vec<u8> = script
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, bytes);
    if encoded.len() > MAX_ENCODED_CHARS {
        return None;
    }
    Some(format!(
        "$__nucleosShell = (Get-Process -Id $PID).Path; & {} {FLAG} --log {} --shell $__nucleosShell --encoded {encoded}",
        quote_powershell(&forward(exe)),
        quote_powershell(&forward(log_dir)),
    ))
}

/// PURE: the tool input a hook should hand back so a foreground Bash or PowerShell command runs
/// through the filter. The CLI replaces the whole input, so every other field is carried over.
/// `None` means "leave the call alone".
pub fn rewrite_input(
    tool_name: &str,
    tool_input: &Value,
    exe: &Path,
    log_dir: &Path,
) -> Option<Value> {
    let object = tool_input.as_object()?;
    let command = object.get("command")?.as_str()?;
    if object.get("run_in_background").and_then(Value::as_bool) == Some(true) {
        return None;
    }
    if command.contains(FLAG) {
        return None;
    }
    let wrapped = match tool_name {
        "Bash" => wrap_bash(command, exe, log_dir)?,
        "PowerShell" => wrap_powershell(command, exe, log_dir)?,
        _ => return None,
    };
    let mut rewritten = object.clone();
    rewritten.insert("command".to_string(), Value::String(wrapped));
    Some(Value::Object(rewritten))
}

const BASH_HEAD: &str = "{ ( ";
const BASH_TAIL: &str = "\n) 2>&1; printf '\\n\\036NUCLEOS-EXIT:%d\\n' \"$?\"; } | ";
const PS_HEAD: &str = "$__nucleosShell = (Get-Process -Id $PID).Path; & ";

/// PURE: the inverse of `rewrite_input`. When `tool_input` carries a command that `wrap_bash` or
/// `wrap_powershell` produced, the same input with the ORIGINAL command; `None` for anything else.
/// PostToolUse reports the rewritten input, while the decision row stores the original.
pub fn original_input(tool_name: &str, tool_input: &Value) -> Option<Value> {
    let object = tool_input.as_object()?;
    let wrapped = object.get("command")?.as_str()?;
    let original = match tool_name {
        "Bash" => {
            let inner = wrapped.strip_prefix(BASH_HEAD)?;
            let tail_at = inner.rfind(BASH_TAIL)?;
            if !inner[tail_at + BASH_TAIL.len()..].contains(FLAG) {
                return None;
            }
            inner[..tail_at].to_string()
        }
        "PowerShell" => {
            let rest = wrapped.strip_prefix(PS_HEAD)?;
            let (_, encoded) = rest.rsplit_once(" --encoded ")?;
            let bytes =
                base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded.trim())
                    .ok()?;
            if bytes.len() % 2 != 0 {
                return None;
            }
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect();
            let script = String::from_utf16(&units).ok()?;
            let suffix = format!("\n{PS_POSTAMBLE}");
            script.strip_suffix(suffix.as_str())?.to_string()
        }
        _ => return None,
    };
    let mut restored = object.clone();
    restored.insert("command".to_string(), Value::String(original));
    Some(Value::Object(restored))
}

/// PURE: the paragraph a Codex run is given, since Codex has no hook that can rewrite a command.
pub fn codex_instruction(exe: &Path, log_dir: &Path, windows: bool) -> String {
    let prefix = if windows {
        format!(
            "& {} {FLAG} --log {} --",
            quote_powershell(&forward(exe)),
            quote_powershell(&forward(log_dir)),
        )
    } else {
        format!(
            "{} {FLAG} --log {} --",
            quote_bash(&forward(exe)),
            quote_bash(&forward(log_dir)),
        )
    };
    format!(
        "When you run a test, check or build command (cargo, npm/pnpm/yarn, pytest, go test, make, \
         gradle/mvn), run it behind this prefix: {prefix} <command>. For example: {prefix} cargo test. \
         The command runs unchanged and exits with the same exit code; you see only its last lines \
         and the failures, and the full log path comes last. Do not use the prefix for interactive \
         or background commands."
    )
}

/// PURE: where one run's raw logs live.
pub fn run_log_dir(data_dir: &Path, run_id: i64) -> PathBuf {
    data_dir.join(RUN_OUTPUT_DIR).join(run_id.to_string())
}

/// Removes the log directories of runs that are gone, or ended more than `retain_days` ago.
/// Directories that are not a run id are left alone. Best effort per directory; returns how many
/// were removed. `retain_days <= 0` keeps everything.
pub async fn prune_logs(
    pool: &SqlitePool,
    data_dir: &Path,
    terminal: &[&str],
    retain_days: i64,
    now: DateTime<Utc>,
) -> std::io::Result<u64> {
    if retain_days <= 0 {
        return Ok(0);
    }
    let entries = match std::fs::read_dir(data_dir.join(RUN_OUTPUT_DIR)) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
    };
    // RFC 3339 on both sides, compared as text, as `runs::lifecycle` does for the transcripts.
    let cutoff = (now - chrono::Duration::days(retain_days)).to_rfc3339();

    let mut removed = 0u64;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(run_id) = entry
            .file_name()
            .to_str()
            .and_then(|n| n.parse::<i64>().ok())
        else {
            continue;
        };
        let row: Result<Option<(String, i64)>, sqlx::Error> = sqlx::query_as(
            "SELECT status, COALESCE(completed_at, created_at) < ? FROM runs WHERE id = ?",
        )
        .bind(&cutoff)
        .bind(run_id)
        .fetch_optional(pool)
        .await;
        let doomed = match row {
            Ok(None) => true,
            Ok(Some((status, past_window))) => {
                past_window != 0 && terminal.contains(&status.as_str())
            }
            Err(error) => {
                tracing::warn!(run_id, %error, "could not look up a run to prune its logs");
                false
            }
        };
        if doomed && std::fs::remove_dir_all(&path).is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use serde_json::json;
    use std::path::{Path, PathBuf};

    fn numbered(prefix: &str, count: usize) -> String {
        (1..=count)
            .map(|n| format!("{prefix} {n}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn lines_of(rendered: &str) -> Vec<&str> {
        rendered.lines().collect()
    }

    #[test]
    fn a_green_run_keeps_the_last_ten_lines_and_the_log_path() {
        let output = numbered("line", 30);
        let log = [PathBuf::from("/data/run-output/7/full.log")];
        let footer = footer(0, 30, Ok(&log));

        let rendered = apply(&output, 0, &footer);
        let lines = lines_of(&rendered);

        for kept in 21..=30 {
            assert!(
                lines.contains(&format!("line {kept}").as_str()),
                "line {kept} was dropped: {rendered}"
            );
        }
        assert!(
            !lines.contains(&"line 20"),
            "an eleventh line survived: {rendered}"
        );
        assert!(!lines.contains(&"line 1"), "the head survived: {rendered}");
        assert!(
            rendered.contains("… 20 lines omitted …"),
            "no omission marker: {rendered}"
        );
        assert!(
            rendered
                .contains("[nucleos] exit 0 · 30 lines · full log: /data/run-output/7/full.log"),
            "no footer: {rendered}"
        );
        assert!(
            rendered.trim_end().ends_with("full.log"),
            "the footer is not last: {rendered}"
        );
    }

    #[test]
    fn the_footer_says_why_a_log_is_missing() {
        let text = footer(3, 12, Err("disk full"));
        assert_eq!(text, "[nucleos] exit 3 · full log unavailable: disk full");
    }

    #[test]
    fn a_red_run_keeps_failure_windows_and_the_tail() {
        let mut lines: Vec<String> = (1..=200).map(|n| format!("ok {n}")).collect();
        lines[49] = "test alpha ... FAILED".to_string();
        lines[149] = "error: could not compile".to_string();
        let output = lines.join("\n");

        let rendered = apply(&output, 1, "FOOTER");
        let kept = lines_of(&rendered);

        assert!(kept.contains(&"test alpha ... FAILED"), "{rendered}");
        assert!(kept.contains(&"error: could not compile"), "{rendered}");
        // The five lines after a match come with it.
        for n in 51..=55 {
            assert!(
                kept.contains(&format!("ok {n}").as_str()),
                "ok {n}: {rendered}"
            );
        }
        assert!(!kept.contains(&"ok 56"), "the window ran long: {rendered}");
        // The last twenty are always kept on a red run.
        for n in 181..=200 {
            assert!(
                kept.contains(&format!("ok {n}").as_str()),
                "ok {n}: {rendered}"
            );
        }
        assert!(!kept.contains(&"ok 180"), "{rendered}");
        // Nothing in the quiet middle.
        assert!(!kept.contains(&"ok 100"), "{rendered}");
        assert!(rendered.contains("lines omitted"), "{rendered}");
        assert_eq!(kept.last(), Some(&"FOOTER"));
    }

    #[test]
    fn a_red_run_without_a_match_keeps_the_last_sixty() {
        let output = numbered("ok", 200);

        let rendered = apply(&output, 2, "FOOTER");
        let kept = lines_of(&rendered);

        for n in 141..=200 {
            assert!(
                kept.contains(&format!("ok {n}").as_str()),
                "ok {n}: {rendered}"
            );
        }
        assert!(!kept.contains(&"ok 140"), "{rendered}");
        assert!(!kept.contains(&"ok 1"), "{rendered}");
        assert!(rendered.contains("… 140 lines omitted …"), "{rendered}");
    }

    #[test]
    fn long_lines_are_cut_at_500_chars() {
        let long = format!("{}\r\nshort", "x".repeat(600));

        let rendered = apply(&long, 0, "FOOTER");

        let expected = format!("{}…", "x".repeat(500));
        assert!(
            lines_of(&rendered).contains(&expected.as_str()),
            "the cut line is wrong: {rendered}"
        );
        assert!(!rendered.contains(&"x".repeat(501)), "{rendered}");
        // A multibyte char on the cut must not split.
        let wide = "é".repeat(700);
        let rendered = apply(&wide, 0, "FOOTER");
        assert!(
            rendered.contains(&format!("{}…", "é".repeat(500))),
            "{rendered}"
        );
        assert!(!rendered.contains(&"é".repeat(501)), "{rendered}");
        // The carriage return of a CRLF line is not printed.
        assert!(!rendered.contains('\r'));
    }

    #[test]
    fn the_bash_wrapper_embeds_the_command_verbatim() {
        let command = "cargo test -- --nocapture\necho 'done' && false";
        let exe = Path::new("/opt/it's/nucleos-core");
        let dir = Path::new("/data/run-output/7");

        let wrapped = wrap_bash(command, exe, dir).unwrap();

        let expected = format!(
            "{{ ( {command}\n) 2>&1; printf '\\n\\036NUCLEOS-EXIT:%d\\n' \"$?\"; }} | \
             '/opt/it'\\''s/nucleos-core' --filter-run --log '/data/run-output/7'"
        );
        assert_eq!(wrapped, expected);
        assert!(wrapped.contains(command));
    }

    #[test]
    fn the_powershell_wrapper_round_trips_the_command() {
        let command = "cargo test\nWrite-Output 'it''s'";
        let exe = Path::new(r"C:\Program Files\nu'cleos\nucleos-core.exe");
        let dir = Path::new(r"C:\Data\run-output\7");

        let wrapped = wrap_powershell(command, exe, dir).unwrap();

        let prefix = "$__nucleosShell = (Get-Process -Id $PID).Path; \
             & 'C:/Program Files/nu''cleos/nucleos-core.exe' --filter-run \
             --log 'C:/Data/run-output/7' --shell $__nucleosShell --encoded ";
        let encoded = wrapped
            .strip_prefix(prefix)
            .unwrap_or_else(|| panic!("unexpected wrapper: {wrapped}"));
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap();
        let units: Vec<u16> = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        let decoded = String::from_utf16(&units).unwrap();

        assert_eq!(decoded, format!("{command}\n{PS_POSTAMBLE}"));
        assert!(PS_POSTAMBLE.contains("exit $global:LASTEXITCODE"));
    }

    #[test]
    fn oversized_background_or_already_wrapped_commands_are_not_wrapped() {
        let exe = Path::new("/bin/nucleos-core");
        let dir = Path::new("/data/run-output/7");

        // Too big for either wrapper.
        let huge = "a".repeat(MAX_WRAPPED_BYTES + 1);
        assert!(wrap_bash(&huge, exe, dir).is_none());
        let long_ps = "Write-Output 1; ".repeat(2_500);
        assert!(wrap_powershell(&long_ps, exe, dir).is_none());

        // rewrite_input: only Bash and PowerShell, only a plain foreground string command.
        let input = json!({"command": "cargo test", "description": "run tests", "timeout": 5000});
        let bash = rewrite_input("Bash", &input, exe, dir).unwrap();
        assert_eq!(bash["description"], "run tests");
        assert_eq!(bash["timeout"], 5000);
        assert_eq!(
            bash["command"].as_str().unwrap(),
            wrap_bash("cargo test", exe, dir).unwrap()
        );
        let ps = rewrite_input("PowerShell", &input, exe, dir).unwrap();
        assert!(ps["command"].as_str().unwrap().contains("--encoded"));

        assert!(rewrite_input("Read", &input, exe, dir).is_none());
        assert!(rewrite_input("Bash", &json!({"command": 7}), exe, dir).is_none());
        assert!(rewrite_input("Bash", &json!({"description": "none"}), exe, dir).is_none());
        let background = json!({"command": "cargo watch", "run_in_background": true});
        assert!(rewrite_input("Bash", &background, exe, dir).is_none());
        let foreground = json!({"command": "cargo test", "run_in_background": false});
        assert!(rewrite_input("Bash", &foreground, exe, dir).is_some());
        let wrapped = json!({"command": format!("x {FLAG} --log y -- cargo test")});
        assert!(rewrite_input("Bash", &wrapped, exe, dir).is_none());
        // Wrapping twice must not happen: the output of the first is refused by the second.
        let once = bash["command"].as_str().unwrap();
        assert!(rewrite_input("Bash", &json!({ "command": once }), exe, dir).is_none());
    }

    #[test]
    fn the_codex_instruction_names_the_prefix_for_each_shell() {
        let exe = Path::new("/bin/nucleos-core");
        let dir = Path::new("/data/run-output/7");

        let unix = codex_instruction(exe, dir, false);
        assert!(unix.contains("'/bin/nucleos-core' --filter-run --log '/data/run-output/7' --"));
        assert!(!unix.contains("& '/bin/nucleos-core'"));
        let windows = codex_instruction(exe, dir, true);
        assert!(
            windows.contains("& '/bin/nucleos-core' --filter-run --log '/data/run-output/7' --")
        );
        assert!(unix.contains("cargo") && unix.contains("pytest"));
    }

    #[test]
    fn the_run_log_dir_sits_under_run_output() {
        let dir = run_log_dir(Path::new("/data"), 42);
        assert_eq!(dir, Path::new("/data").join(RUN_OUTPUT_DIR).join("42"));
    }

    async fn insert_run(pool: &sqlx::SqlitePool, status: &str, completed_at: Option<&str>) -> i64 {
        sqlx::query_scalar(
            "INSERT INTO runs (project_id, cwd, prompt, status, created_at, completed_at)
             VALUES ('proj', '/tmp', 'ask', ?, '2026-01-01T00:00:00+00:00', ?)
             RETURNING id",
        )
        .bind(status)
        .bind(completed_at)
        .fetch_one(pool)
        .await
        .unwrap()
    }

    fn make_log(data_dir: &Path, name: &str) -> PathBuf {
        let dir = data_dir.join(RUN_OUTPUT_DIR).join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("full.log"), "output").unwrap();
        dir
    }

    #[tokio::test]
    async fn prune_removes_only_logs_of_ended_runs_past_the_window() {
        let pool = crate::testdb::fresh_pool().await;
        let now = chrono::DateTime::parse_from_rfc3339("2026-08-08T12:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let data = tempfile::tempdir().unwrap();
        let terminal = ["completed", "failed"];

        // A missing run-output directory is not an error.
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(
            prune_logs(&pool, empty.path(), &terminal, 30, now)
                .await
                .unwrap(),
            0
        );

        let old = insert_run(&pool, "completed", Some("2026-06-01T00:00:00+00:00")).await;
        let recent = insert_run(&pool, "completed", Some("2026-08-07T00:00:00+00:00")).await;
        let live = insert_run(&pool, "running", None).await;
        let old_dir = make_log(data.path(), &old.to_string());
        let recent_dir = make_log(data.path(), &recent.to_string());
        let live_dir = make_log(data.path(), &live.to_string());
        let orphan_dir = make_log(data.path(), "999999");
        let stray_dir = make_log(data.path(), "notes");

        // A window of zero days means "keep everything".
        assert_eq!(
            prune_logs(&pool, data.path(), &terminal, 0, now)
                .await
                .unwrap(),
            0
        );
        assert!(old_dir.exists() && orphan_dir.exists());

        let removed = prune_logs(&pool, data.path(), &terminal, 30, now)
            .await
            .unwrap();

        assert_eq!(removed, 2, "the old run and the orphan go, nothing else");
        assert!(
            !old_dir.exists(),
            "an ended run past the window kept its log"
        );
        assert!(!orphan_dir.exists(), "a log with no run row survived");
        assert!(recent_dir.exists(), "a run inside the window lost its log");
        assert!(live_dir.exists(), "a run still going lost its log");
        assert!(
            stray_dir.exists(),
            "a directory that is not a run id was removed"
        );
    }

    #[test]
    fn original_input_inverts_both_wrappers() {
        let command = "echo \"it's\" 'x'\nprintf 'héllo → ✓'";
        let exe = Path::new("/bin/nucleos-core");
        let logs = Path::new("/data/run-output/3");
        for tool in ["Bash", "PowerShell"] {
            let input = json!({"command": command, "description": "d", "timeout": 5});
            let rewritten = rewrite_input(tool, &input, exe, logs).expect("wrapped");
            assert_ne!(rewritten, input);
            assert_eq!(original_input(tool, &rewritten), Some(input), "{tool}");
        }
    }

    #[test]
    fn original_input_ignores_an_ordinary_command() {
        let input = json!({"command": "cargo test"});
        assert_eq!(original_input("Bash", &input), None);
        assert_eq!(original_input("PowerShell", &input), None);
        assert_eq!(original_input("Read", &input), None);
        assert_eq!(original_input("Bash", &json!({"file_path": "x"})), None);
    }
}
