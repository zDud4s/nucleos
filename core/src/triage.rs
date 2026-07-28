//! The email pillar's run machinery (spec §5): the sandbox a triage run executes in, and the
//! startup verification that proves its hook barrier is really in force.
//!
//! Split from `email.rs` deliberately. That module owns the domain — what a message is, when it is
//! noise, how a batch is stored. This one owns what happens when a run is launched over that
//! content, which is a different concern with a different failure mode: there, a bug loses mail;
//! here, a bug hands a stranger's text a shell.
//!
//! Spec §5.5 asks for TWO independent barriers, and the reason is written into the shape of this
//! file. Barrier 1 is `ToolPolicy::None` in `runner.rs` — the CLI refuses on its own. Barrier 2 is
//! the `PreToolUse` hook, which is COOPERATIVE: it only runs if the `.claude/settings.json`
//! resolved from the run's working directory registers it. That is why the run gets a sandbox of
//! its own rather than inheriting the daemon's working directory, which is the NucleOS repository.

use std::path::{Path, PathBuf};

/// The reason `hooks.rs` gives when it denies a triage run. The startup verification asserts on
/// this exact string, which is the whole reason it is a constant: every fail-closed path in
/// `ask_daemon.py` also answers `block`, so only the REASON can tell "the barrier works" apart
/// from "the daemon was unreachable".
pub const TRIAGE_DENY_REASON: &str = "email triage runs have no tools";

/// The production hook, compiled in rather than read from disk at runtime.
///
/// Copying `<cwd>/.claude/hooks/ask_daemon.py` at startup would reintroduce exactly the dependency
/// on the repository root that the sandbox exists to remove, and it would find nothing at all in a
/// packaged install.
const HOOK_SCRIPT: &str = include_str!("../../.claude/hooks/ask_daemon.py");

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
                    "command": format!("python \"{}\"", script_path.display()),
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
async fn probe_hook(sandbox: &Path, env: &[(&str, String)]) -> Result<String, BarrierError> {
    use tokio::io::AsyncWriteExt;

    let script = sandbox.join("hooks").join("ask_daemon.py");
    // A tool the classifier would ALLOW. That is the point: with the branch absent this payload
    // comes back `allow`, so a pass cannot be an accident of picking something dangerous.
    let payload = serde_json::json!({
        "tool_name": "Read",
        "tool_input": { "file_path": "startup-verification" },
    })
    .to_string();

    let mut command = tokio::process::Command::new("python");
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
    .bind(crate::email::TRIAGE_MODE)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sandbox_has_exactly_the_two_files_a_run_needs() {
        let dir = tempfile::tempdir().unwrap();
        ensure_sandbox(dir.path()).unwrap();

        let script = dir.path().join("hooks").join("ask_daemon.py");
        assert!(script.exists());
        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        let entry = &settings["hooks"]["PreToolUse"][0];
        assert_eq!(
            entry["matcher"], "*",
            "a Bash-only matcher would let Read, Write and Edit past the hook entirely"
        );
        let command = entry["hooks"][0]["command"].as_str().unwrap();
        assert!(
            command.contains(&script.display().to_string()),
            "the hook command must carry an absolute path: {command}"
        );
        assert!(
            !command.contains("CLAUDE_PROJECT_DIR"),
            "the sandbox must not depend on being recognised as a project: {command}"
        );
    }

    #[test]
    fn building_the_sandbox_twice_is_the_same_sandbox() {
        let dir = tempfile::tempdir().unwrap();
        ensure_sandbox(dir.path()).unwrap();
        let first = std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
        ensure_sandbox(dir.path()).unwrap();
        let second = std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap();
        assert_eq!(first, second);
    }

    /// The sandbox is repaired on every start rather than trusted, so an edit that would silently
    /// disable the hook does not survive a restart.
    #[test]
    fn a_corrupted_sandbox_is_repaired() {
        let dir = tempfile::tempdir().unwrap();
        ensure_sandbox(dir.path()).unwrap();
        std::fs::write(dir.path().join(".claude/settings.json"), "{}").unwrap();
        std::fs::write(dir.path().join("hooks/ask_daemon.py"), "print('allow')").unwrap();

        ensure_sandbox(dir.path()).unwrap();
        let settings: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join(".claude/settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(settings["hooks"]["PreToolUse"][0]["matcher"], "*");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("hooks/ask_daemon.py")).unwrap(),
            HOOK_SCRIPT
        );
    }

    #[test]
    fn only_the_branch_reason_counts_as_proof() {
        assert_eq!(
            interpret_barrier_probe(
                &serde_json::json!({"decision": "block", "reason": TRIAGE_DENY_REASON}).to_string()
            ),
            Ok(())
        );
    }

    /// Every fail-closed path in `ask_daemon.py` also answers `block`. Accepting any block is the
    /// exact false green the spec's fourth review round caught.
    #[test]
    fn a_fail_closed_block_is_not_proof() {
        let probe = serde_json::json!({
            "decision": "block",
            "reason": "daemon unreachable or errored (…) - failing closed",
        })
        .to_string();
        assert!(matches!(
            interpret_barrier_probe(&probe),
            Err(BarrierError::WrongReason(_))
        ));
    }

    #[test]
    fn an_allow_is_a_failed_verification() {
        let probe = serde_json::json!({
            "hookSpecificOutput": {
                "hookEventName": "PreToolUse",
                "permissionDecision": "allow",
                "permissionDecisionReason": "autopilot: allowed",
            }
        })
        .to_string();
        assert_eq!(
            interpret_barrier_probe(&probe),
            Err(BarrierError::NotBlocked)
        );
    }

    /// Silence is what the script produces with no `NUCLEOS_RUN_ID`, and it proves nothing.
    #[test]
    fn silence_is_a_failed_verification() {
        assert_eq!(interpret_barrier_probe("   "), Err(BarrierError::NoOpinion));
        assert_eq!(interpret_barrier_probe(""), Err(BarrierError::NoOpinion));
    }

    async fn test_state() -> crate::state::AppState {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(5)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        crate::state::AppState {
            token: crate::auth::Token("verification-token".into()),
            pool,
            runner: std::sync::Arc::new(crate::runner::FakeCommandRunner::default()),
            run_handles: std::sync::Arc::new(std::sync::Mutex::new(
                std::collections::HashMap::new(),
            )),
            email: std::sync::Arc::new(crate::state::EmailRuntime::default()),
            run_timeout: crate::state::DEFAULT_RUN_TIMEOUT,
        }
    }

    /// Serves `router` on an ephemeral port and returns its base URL. The port is the injection
    /// point that makes the negative cases expressible.
    async fn serve(router: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
        tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        url
    }

    #[tokio::test]
    async fn the_barrier_verifies_against_the_real_router() {
        let dir = tempfile::tempdir().unwrap();
        ensure_sandbox(dir.path()).unwrap();
        let state = test_state().await;
        let pool = state.pool.clone();
        let url = serve(crate::http::build_router(state)).await;

        assert_eq!(
            verify_hook_barrier(&pool, dir.path(), &url, "verification-token").await,
            Ok(())
        );
    }

    /// The verification's own bookkeeping: it must not leave the row it invented behind, or every
    /// restart adds one to a table the triage loop counts over.
    #[tokio::test]
    async fn verification_leaves_no_row_behind() {
        let dir = tempfile::tempdir().unwrap();
        ensure_sandbox(dir.path()).unwrap();
        let state = test_state().await;
        let pool = state.pool.clone();
        let url = serve(crate::http::build_router(state)).await;

        verify_hook_barrier(&pool, dir.path(), &url, "verification-token")
            .await
            .unwrap();
        let left: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM runs")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }

    /// The branch deleted or misspelled: a daemon that answers `allow` must fail the check. This is
    /// the case the obvious implementation could not detect.
    #[tokio::test]
    async fn a_daemon_that_allows_fails_the_verification() {
        let dir = tempfile::tempdir().unwrap();
        ensure_sandbox(dir.path()).unwrap();
        let state = test_state().await;
        let pool = state.pool.clone();

        let stub = axum::Router::new().route(
            "/hooks/pretooluse-decision",
            axum::routing::post(|| async {
                axum::Json(serde_json::json!({"decision": "allow", "reason": "sure"}))
            }),
        );
        let url = serve(stub).await;

        assert_eq!(
            verify_hook_barrier(&pool, dir.path(), &url, "verification-token").await,
            Err(BarrierError::NotBlocked)
        );
    }

    /// Running the check before the listener is up catches `ask_daemon.py`'s unreachable path,
    /// which answers `block` — a pass here would mean the check proves nothing about the branch.
    #[tokio::test]
    async fn an_unreachable_daemon_fails_the_verification() {
        let dir = tempfile::tempdir().unwrap();
        ensure_sandbox(dir.path()).unwrap();
        let state = test_state().await;

        // Bind and immediately drop, so the port is one nothing is listening on.
        let closed = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://127.0.0.1:{}", closed.local_addr().unwrap().port());
        drop(closed);

        assert!(matches!(
            verify_hook_barrier(&state.pool, dir.path(), &url, "verification-token").await,
            Err(BarrierError::WrongReason(_))
        ));
    }

    #[tokio::test]
    async fn a_probe_without_a_run_id_fails_the_verification() {
        let dir = tempfile::tempdir().unwrap();
        ensure_sandbox(dir.path()).unwrap();
        let state = test_state().await;
        let url = serve(crate::http::build_router(state)).await;

        let probe = probe_hook(
            dir.path(),
            &[
                ("NUCLEOS_DAEMON_URL", url),
                ("NUCLEOS_DAEMON_TOKEN", "verification-token".to_string()),
            ],
        )
        .await
        .unwrap();
        assert_eq!(
            interpret_barrier_probe(&probe),
            Err(BarrierError::NoOpinion)
        );
    }

    /// An id no run carries falls through to `mode = "real"`, where the classifier ALLOWS `Read`.
    /// So a verification that forgot to insert its row would be testing the classifier, not the
    /// barrier — and would fail for a reason that has nothing to do with the hook.
    #[tokio::test]
    async fn a_probe_naming_an_unknown_run_fails_the_verification() {
        let dir = tempfile::tempdir().unwrap();
        ensure_sandbox(dir.path()).unwrap();
        let state = test_state().await;
        let url = serve(crate::http::build_router(state)).await;

        let probe = probe_hook(
            dir.path(),
            &[
                ("NUCLEOS_RUN_ID", "424242".to_string()),
                ("NUCLEOS_DAEMON_URL", url),
                ("NUCLEOS_DAEMON_TOKEN", "verification-token".to_string()),
            ],
        )
        .await
        .unwrap();
        assert_eq!(
            interpret_barrier_probe(&probe),
            Err(BarrierError::NotBlocked)
        );
    }

    #[test]
    fn output_that_is_not_a_decision_is_a_failed_verification() {
        assert!(matches!(
            interpret_barrier_probe("Traceback (most recent call last):"),
            Err(BarrierError::Unreadable(_))
        ));
    }
}
