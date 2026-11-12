//! The triage sandbox and its hook-barrier verification.

use std::path::{Path, PathBuf};

/// The reason `hooks.rs` gives when it denies a triage run. The startup verification asserts on
/// this exact string, which is the whole reason it is a constant: every fail-closed path in
/// `ask_daemon.py` also answers `block`, so only the REASON can tell "the barrier works" apart
/// from "the daemon was unreachable".
pub const TRIAGE_DENY_REASON: &str = "email triage runs have no tools";

/// The production hook, compiled in rather than read from disk at runtime: the SAME source
/// `autopilot.rs`'s `HOOK_SOURCE` embeds, `core/hooks/ask_daemon.py`, so the email sandbox and
/// every project the daemon wires run one script. `.claude/hooks/ask_daemon.py` is a tracked copy
/// of it, held equal by `scripts/test-hook-filter.py`, and is not compiled in.
///
/// Copying `<cwd>/.claude/hooks/ask_daemon.py` at startup would reintroduce exactly the dependency
/// on the repository root that the sandbox exists to remove, and it would find nothing at all in a
/// packaged install.
///
/// One behaviour changed when this moved off the `.claude/` copy, accepted by the owner on
/// 2026-09-14: a call whose payload says `hook_event_name` is `PostToolUse` or
/// `PostToolUseFailure` goes to `report_outcome`, which never decides, so the sandbox gives it no
/// decision instead of refusing it. The sandbox registers the hook only under `PreToolUse`, the
/// field is written by the Claude CLI and not by the model, and barrier 1 (`ToolPolicy::None`)
/// still refuses every tool.
pub(super) const HOOK_SCRIPT: &str = include_str!("../../../hooks/ask_daemon.py");

/// Builds the directory a triage run works in, and returns it.
///
/// Idempotent by rewriting rather than by checking: the daemon starts often, the files are small,
/// and a sandbox whose `settings.json` was edited by hand is repaired instead of trusted. The
/// hook command carries an ABSOLUTE path — not `${CLAUDE_PROJECT_DIR}` — so it does not depend on
/// the CLI considering this directory a project at all.
pub fn ensure_sandbox(root: &Path) -> std::io::Result<PathBuf> {
    let hooks_dir = root.join("hooks");
    let claude_dir = root.join(".claude");
    std::fs::create_dir_all(&hooks_dir)?;
    std::fs::create_dir_all(&claude_dir)?;

    let script_path = hooks_dir.join("ask_daemon.py");
    std::fs::write(&script_path, HOOK_SCRIPT)?;

    // `"*"`, never `"Bash"`. The proof-of-concept fixture matches Bash because it was written to
    // demonstrate blocking; with that matcher, Read/Edit/Write/Grep/Glob would never reach the hook
    // at all, and every one of them is a way for a mail body to act.
    let settings = serde_json::json!({
        "hooks": {
            "PreToolUse": [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": format!("{} \"{}\"", crate::governance::autopilot::HOOK_INTERPRETER, script_path.display()),
                }],
            }],
        }
    });
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::to_string_pretty(&settings)?,
    )?;

    Ok(root.to_path_buf())
}

/// Why the hook barrier could not be shown to work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BarrierError {
    /// The hook allowed a tool the classifier would also have allowed — the branch is missing,
    /// misspelled, or never reached.
    NotBlocked,
    /// It blocked, but for one of `ask_daemon.py`'s fail-closed reasons rather than the branch's.
    /// A daemon that is not listening yet produces exactly this, which is why a check that only
    /// looked at the decision would pass while proving nothing.
    WrongReason(String),
    /// The script said nothing at all, which is what it does with no `NUCLEOS_RUN_ID`.
    NoOpinion,
    /// The script produced something that is not a decision.
    Unreadable(String),
    /// The script could not be run.
    NotRunnable(String),
}

impl std::fmt::Display for BarrierError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BarrierError::NotBlocked => {
                write!(f, "the hook allowed a tool a triage run must never get")
            }
            BarrierError::WrongReason(reason) => write!(
                f,
                "the hook blocked for an unrelated reason ({reason}), so the triage branch is unproven"
            ),
            BarrierError::NoOpinion => write!(f, "the hook expressed no opinion"),
            BarrierError::Unreadable(out) => write!(f, "the hook produced no decision ({out})"),
            BarrierError::NotRunnable(err) => write!(f, "the hook script could not be run ({err})"),
        }
    }
}

/// PURE: what one probe of the hook script means.
///
/// Separated from running it because this is where the fourth review round found the check could
/// not fail: `ask_daemon.py` answers `block` on EVERY fail-closed path, so a verification that
/// accepted any `block` would pass with the branch deleted, with the daemon down, with the payload
/// wrong. Only this exact reason proves the branch ran.
pub fn interpret_barrier_probe(stdout: &str) -> Result<(), BarrierError> {
    let stdout = stdout.trim();
    if stdout.is_empty() {
        return Err(BarrierError::NoOpinion);
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout) else {
        return Err(BarrierError::Unreadable(stdout.to_string()));
    };

    if value.get("decision").and_then(|d| d.as_str()) == Some("block") {
        let reason = value
            .get("reason")
            .and_then(|r| r.as_str())
            .unwrap_or_default();
        return if reason == TRIAGE_DENY_REASON {
            Ok(())
        } else {
            Err(BarrierError::WrongReason(reason.to_string()))
        };
    }

    // The approval contract, or anything else: either way the tool was not refused.
    if value.get("hookSpecificOutput").is_some() {
        return Err(BarrierError::NotBlocked);
    }
    Err(BarrierError::Unreadable(stdout.to_string()))
}

/// Runs the sandbox's hook script once with the given environment and returns its stdout.
pub async fn probe_hook(sandbox: &Path, env: &[(&str, String)]) -> Result<String, BarrierError> {
    use tokio::io::AsyncWriteExt;

    let script = sandbox.join("hooks").join("ask_daemon.py");
    // A tool the classifier would ALLOW. That is the point: with the branch absent this payload
    // comes back `allow`, so a pass cannot be an accident of picking something dangerous.
    let payload = serde_json::json!({
        "tool_name": "Read",
        "tool_input": { "file_path": "startup-verification" },
    })
    .to_string();

    let mut command = tokio::process::Command::new(crate::governance::autopilot::HOOK_INTERPRETER);
    command
        .arg(&script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }

    let mut child = command
        .spawn()
        .map_err(|error| BarrierError::NotRunnable(error.to_string()))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(payload.as_bytes())
            .await
            .map_err(|error| BarrierError::NotRunnable(error.to_string()))?;
    }
    let output = child
        .wait_with_output()
        .await
        .map_err(|error| BarrierError::NotRunnable(error.to_string()))?;
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Proves barrier 2 is in force, by making the hook refuse a tool it could only refuse through the
/// triage branch (spec §5.5).
///
/// MUST run after the listener is serving. Run any earlier and it tests the "daemon unreachable"
/// path instead — which also answers `block`, and would therefore pass while proving nothing.
///
/// `daemon_url` is a parameter rather than a hardcoded loopback address so the negative cases can
/// exist at all: pointed at a stub that answers `allow`, or at nothing, this must report failure.
pub async fn verify_hook_barrier(
    pool: &sqlx::SqlitePool,
    sandbox: &Path,
    daemon_url: &str,
    token: &str,
) -> Result<(), BarrierError> {
    // A throwaway run whose only job is to carry the mode. It is deliberately NOT in
    // `run_handles`: the branch has to hold for a run nothing is executing under, which is the
    // fallthrough this pillar had to close.
    let run_id = sqlx::query(
        "INSERT INTO runs (prompt, status, mode, created_at)
         VALUES ('startup barrier verification', 'completed', ?, ?)",
    )
    .bind(crate::integrations::email::TRIAGE_MODE)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await
    .map_err(|error| BarrierError::NotRunnable(error.to_string()))?
    .last_insert_rowid();

    let probe = probe_hook(
        sandbox,
        &[
            ("NUCLEOS_RUN_ID", run_id.to_string()),
            ("NUCLEOS_DAEMON_URL", daemon_url.to_string()),
            ("NUCLEOS_DAEMON_TOKEN", token.to_string()),
        ],
    )
    .await;

    let _ = sqlx::query("DELETE FROM runs WHERE id = ?")
        .bind(run_id)
        .execute(pool)
        .await;

    interpret_barrier_probe(&probe?)
}
