use async_trait::async_trait;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

// `cost_usd` is an `f64`, which is not `Eq`, so `RunOutcome` can only derive `PartialEq`.
#[derive(Debug, Clone, PartialEq)]
pub struct RunOutcome {
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    /// Captured from the CLI's initial `stream-json` `init` message (spec §3.3) — known as soon as the
    /// run starts, so it survives a run terminated mid-flight (cancel / timeout / awaiting_approval).
    pub session_id: Option<String>,
    /// Captured from the CLI's final `result` message — only known when the run ends (spec §3.3/§8.5).
    pub cost_usd: Option<f64>,
}

/// Barrier 1 of the two-barrier tool model: a restriction the CLI enforces on itself, so it holds
/// where the `PreToolUse` hook cannot. The hook is COOPERATIVE — it only runs if the
/// `.claude/settings.json` resolved from the run's working directory registers it — so it is not a
/// tool boundary on its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPolicy {
    /// Every tool the CLI offers, governed at runtime by the hook + classifier (spec §8.2).
    Unrestricted,
    /// Only the NucleOS MCP server: every built-in denied, every ambient MCP server dropped.
    McpOnly,
}

/// Built-in tool names denied under `ToolPolicy::McpOnly`.
///
/// A blocklist, and deliberately so: measured against CLI 2.1.198, `--allowedTools` does not
/// restrict anything — it only GRANTS permission on top of what is already allowed — and a
/// deny-all `--disallowedTools "*"` takes the MCP server down with the built-ins, which would
/// leave the orchestrator with nothing to call. Denying a name this CLI does not have is harmless,
/// so the list is wider than any one version's tool set; a CLI upgrade that ADDS a tool still
/// needs this reviewed. A name the CLI does not know costs one stderr line per run
/// (`Permission deny rule "X" matches no known tool`) — that warning is the price of the margin,
/// not a typo to clean up.
const BUILTIN_TOOLS: &[&str] = &[
    "Agent",
    "Artifact",
    "Bash",
    "BashOutput",
    "CronCreate",
    "CronDelete",
    "CronList",
    "DesignSync",
    "Edit",
    "EnterPlanMode",
    "EnterWorktree",
    "ExitPlanMode",
    "ExitWorktree",
    "Glob",
    "Grep",
    "KillShell",
    "ListMcpResourcesTool",
    "LSP",
    "Monitor",
    "NotebookEdit",
    "PowerShell",
    "PushNotification",
    "Read",
    "ReadMcpResourceDirTool",
    "ReadMcpResourceTool",
    "RemoteTrigger",
    "ReportFindings",
    "ScheduleWakeup",
    "SendMessage",
    "Skill",
    "SlashCommand",
    "Task",
    "TaskOutput",
    "TaskStop",
    "TodoWrite",
    "ToolSearch",
    "WebFetch",
    "WebSearch",
    "Workflow",
    "Write",
];

/// The full `claude` argument vector for one run. Pure, so the flags that decide what a run can
/// reach are asserted in tests instead of inspected on a live process.
pub(crate) fn cli_args(
    prompt: &str,
    model: &str,
    plan_only: bool,
    resume_session_id: Option<&str>,
    mcp_config: Option<&Path>,
    tool_policy: ToolPolicy,
) -> Vec<String> {
    let mut args = vec![
        "-p".to_string(),
        prompt.to_string(),
        "--model".to_string(),
        model.to_string(),
    ];
    if let Some(sid) = resume_session_id {
        args.push("--resume".to_string());
        args.push(sid.to_string());
    }
    args.push("--output-format".to_string());
    args.push("stream-json".to_string());
    args.push("--verbose".to_string());
    if plan_only {
        args.push("--permission-mode".to_string());
        args.push("plan".to_string());
    }
    if let Some(path) = mcp_config {
        args.push("--mcp-config".to_string());
        args.push(path.to_string_lossy().into_owned());
        args.push("--allowedTools".to_string());
        args.push("mcp__nucleos__*".to_string());
    }
    if tool_policy == ToolPolicy::McpOnly {
        // Drops every MCP server this user happens to have configured — the ambient surface a
        // spawned run inherits otherwise includes file-writing connectors.
        args.push("--strict-mcp-config".to_string());
        args.push("--disallowedTools".to_string());
        args.push(BUILTIN_TOOLS.join(","));
    }
    args
}

#[async_trait]
pub trait CommandRunner: Send + Sync {
    /// Runs one `claude -p` invocation. `cwd`, when set, is the run's working directory (spec §3.3).
    /// `session_tx` receives the `session_id` the instant the CLI's `init` message is parsed.
    ///
    /// Deliberately wide rather than taking an options struct: every parameter is one CLI flag, and
    /// keeping them positional means adding a flag cannot silently inherit a default nobody chose.
    #[allow(clippy::too_many_arguments)]
    async fn run_prompt(
        &self,
        prompt: &str,
        env: &[(String, String)],
        cwd: Option<&Path>,
        plan_only: bool,
        resume_session_id: Option<&str>,
        mcp_config: Option<&Path>,
        tool_policy: ToolPolicy,
        session_tx: UnboundedSender<String>,
    ) -> std::io::Result<RunOutcome>;
}

pub struct ClaudeCliRunner {
    pub model: String,
}

#[async_trait]
impl CommandRunner for ClaudeCliRunner {
    async fn run_prompt(
        &self,
        prompt: &str,
        env: &[(String, String)],
        cwd: Option<&Path>,
        plan_only: bool,
        resume_session_id: Option<&str>,
        mcp_config: Option<&Path>,
        tool_policy: ToolPolicy,
        session_tx: UnboundedSender<String>,
    ) -> std::io::Result<RunOutcome> {
        // The Claude Code CLI binary. Overridable via `NUCLEOS_CLAUDE_BIN` because on Windows the
        // npm-installed `claude` is a `.cmd` shim that Rust's `Command` can't spawn by name — the
        // daemon points this at the real `claude.exe`. Defaults to `claude` where it's on PATH.
        let claude_bin =
            std::env::var("NUCLEOS_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string());
        let mut cmd = Command::new(&claude_bin);
        cmd.args(cli_args(
            prompt,
            &self.model,
            plan_only,
            resume_session_id,
            mcp_config,
            tool_policy,
        ));
        for (k, v) in env {
            cmd.env(k, v);
        }
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        // kill_on_drop turns an aborted awaiting-task (cancel/timeout, Task 4) into the OS `claude`
        // process actually dying. DO NOT drop this line — Chunk 5 Task 1 adds `--model` and preserves it.
        cmd.kill_on_drop(true);

        let mut child = cmd.spawn()?;
        let stdout = child.stdout.take().expect("stdout piped above");
        let stderr = child.stderr.take().expect("stderr piped above");

        // Drain stderr in a *concurrent* task — reading only stdout while the child writes stderr would
        // deadlock the moment the OS stderr pipe buffer fills (spec §3.3's explicit trap).
        let stderr_task = tokio::spawn(async move {
            let mut buf = String::new();
            let mut reader = BufReader::new(stderr);
            let _ = reader.read_to_string(&mut buf).await;
            buf
        });

        let mut lines = BufReader::new(stdout).lines();
        let mut stdout_acc = String::new();
        let mut session_id: Option<String> = None;
        let mut cost_usd: Option<f64> = None;

        while let Some(line) = lines.next_line().await? {
            stdout_acc.push_str(&line);
            stdout_acc.push('\n');
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                if session_id.is_none()
                    && let Some(sid) = v.get("session_id").and_then(|x| x.as_str())
                {
                    session_id = Some(sid.to_string());
                    // Best-effort: the receiver may already be gone if the run was cancelled.
                    let _ = session_tx.send(sid.to_string());
                }
                if v.get("type").and_then(|x| x.as_str()) == Some("result")
                    && let Some(c) = v
                        .get("total_cost_usd")
                        .or_else(|| v.get("cost_usd"))
                        .and_then(|x| x.as_f64())
                {
                    cost_usd = Some(c);
                }
            }
        }

        let status = child.wait().await?;
        let stderr_str = stderr_task.await.unwrap_or_default();

        Ok(RunOutcome {
            exit_code: status.code().unwrap_or(-1),
            stdout: stdout_acc,
            stderr: stderr_str,
            session_id,
            cost_usd,
        })
    }
}

/// The test double for `CommandRunner`. `#[cfg(test)]` because every user of it is a test — building
/// it into the daemon would ship a runner that can fake a run's outcome.
#[cfg(test)]
#[derive(Default)]
pub struct FakeCommandRunner {
    pub canned: std::sync::Mutex<Option<RunOutcome>>,
    // Set by Task 4's cancellation/timeout tests to simulate a slow/hung run.
    pub delay: std::sync::Mutex<Option<std::time::Duration>>,
    pub last_plan_only: std::sync::Mutex<Option<bool>>,
    pub last_cwd: std::sync::Mutex<Option<std::path::PathBuf>>,
    pub last_resume: std::sync::Mutex<Option<String>>,
    pub last_mcp_config: std::sync::Mutex<Option<std::path::PathBuf>>,
    pub last_tool_policy: std::sync::Mutex<Option<ToolPolicy>>,
    /// Test-only: return an `Err` (simulated launch failure — no work done) for the first N calls.
    pub fail_times: std::sync::Mutex<u32>,
    /// Test-only: count of run_prompt invocations.
    pub calls: std::sync::Mutex<u32>,
}

#[cfg(test)]
#[async_trait]
impl CommandRunner for FakeCommandRunner {
    async fn run_prompt(
        &self,
        _prompt: &str,
        _env: &[(String, String)],
        cwd: Option<&Path>,
        plan_only: bool,
        resume_session_id: Option<&str>,
        mcp_config: Option<&Path>,
        tool_policy: ToolPolicy,
        session_tx: UnboundedSender<String>,
    ) -> std::io::Result<RunOutcome> {
        {
            *self.calls.lock().unwrap() += 1;
        }
        {
            let mut remaining = self.fail_times.lock().unwrap();
            if *remaining > 0 {
                *remaining -= 1;
                return Err(std::io::Error::other("fake launch failure"));
            }
        }
        *self.last_cwd.lock().unwrap() = cwd.map(|c| c.to_path_buf());
        *self.last_plan_only.lock().unwrap() = Some(plan_only);
        *self.last_resume.lock().unwrap() = resume_session_id.map(|s| s.to_string());
        *self.last_mcp_config.lock().unwrap() = mcp_config.map(|p| p.to_path_buf());
        *self.last_tool_policy.lock().unwrap() = Some(tool_policy);
        // Clone the canned outcome in its own scope so the MutexGuard drops before any `.await`.
        let outcome = {
            let guard = self.canned.lock().unwrap();
            guard.clone().unwrap_or(RunOutcome {
                exit_code: 0,
                stdout: "fake output".into(),
                stderr: String::new(),
                session_id: Some("fake-session-id".into()),
                cost_usd: Some(0.0),
            })
        };
        // Emit session_id *before* any simulated delay — mirrors the real CLI's early `init` message.
        if let Some(sid) = &outcome.session_id {
            let _ = session_tx.send(sid.clone());
        }
        let delay = *self.delay.lock().unwrap();
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn fake_runner_returns_canned_outcome() {
        let runner = FakeCommandRunner {
            canned: std::sync::Mutex::new(Some(RunOutcome {
                exit_code: 0,
                stdout: "42".into(),
                stderr: String::new(),
                session_id: Some("sess-1".into()),
                cost_usd: Some(1.5),
            })),
            last_plan_only: std::sync::Mutex::new(None),
            ..Default::default()
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let outcome = runner
            .run_prompt(
                "what is 6*7",
                &[],
                None,
                false,
                None,
                None,
                ToolPolicy::Unrestricted,
                tx,
            )
            .await
            .unwrap();
        assert_eq!(outcome.stdout, "42");
        assert_eq!(outcome.exit_code, 0);
        assert_eq!(outcome.cost_usd, Some(1.5));
    }

    #[tokio::test]
    async fn fake_runner_emits_session_id_before_returning() {
        let runner = FakeCommandRunner::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = tokio::spawn(async move {
            runner
                .run_prompt(
                    "hi",
                    &[],
                    None,
                    false,
                    None,
                    None,
                    ToolPolicy::Unrestricted,
                    tx,
                )
                .await
        });
        let sid = rx.recv().await;
        assert_eq!(sid.as_deref(), Some("fake-session-id"));
        let outcome = handle.await.unwrap().unwrap();
        assert_eq!(outcome.session_id.as_deref(), Some("fake-session-id"));
    }

    #[tokio::test]
    async fn fake_runner_records_the_resume_session_id() {
        let runner = FakeCommandRunner::default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        runner
            .run_prompt(
                "resume please",
                &[],
                None,
                false,
                Some("sess-9"),
                None,
                ToolPolicy::Unrestricted,
                tx,
            )
            .await
            .unwrap();
        assert_eq!(
            *runner.last_resume.lock().unwrap(),
            Some("sess-9".to_string())
        );
    }

    #[tokio::test]
    async fn fake_runner_records_the_mcp_config() {
        let runner = FakeCommandRunner::default();
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        runner
            .run_prompt(
                "use mcp",
                &[],
                None,
                false,
                None,
                Some(std::path::Path::new("C:/tmp/mcp.json")),
                ToolPolicy::McpOnly,
                tx,
            )
            .await
            .unwrap();
        assert_eq!(
            *runner.last_mcp_config.lock().unwrap(),
            Some(std::path::PathBuf::from("C:/tmp/mcp.json"))
        );
    }

    fn args_for(policy: ToolPolicy, mcp: Option<&Path>) -> Vec<String> {
        cli_args("triage this", "sonnet", false, None, mcp, policy)
    }

    /// An autopilot run keeps the full tool set — the hook and the classifier are what govern it,
    /// and denying tools here would break every real run.
    #[test]
    fn unrestricted_adds_no_tool_restriction_flags() {
        let args = args_for(ToolPolicy::Unrestricted, None);
        assert!(!args.iter().any(|a| a == "--disallowedTools"));
        assert!(!args.iter().any(|a| a == "--strict-mcp-config"));
    }

    /// The orchestrator reaches NucleOS through its MCP server and must reach nothing else. This is
    /// the property the `--allowedTools` line alone was wrongly believed to provide.
    #[test]
    fn mcp_only_denies_the_built_in_tools() {
        let args = args_for(ToolPolicy::McpOnly, None);
        let denied = args
            .windows(2)
            .find(|w| w[0] == "--disallowedTools")
            .map(|w| w[1].clone())
            .expect("McpOnly must deny built-ins");
        for tool in [
            "Read",
            "Bash",
            "Write",
            "Edit",
            "Task",
            "WebFetch",
            "PowerShell",
        ] {
            assert!(
                denied.split(',').any(|t| t == tool),
                "{tool} must be denied under McpOnly"
            );
        }
    }

    /// Every MCP server the user happens to have configured is ambient to a spawned run, including
    /// file-writing connectors. Only the server the daemon passes in may survive.
    #[test]
    fn mcp_only_drops_the_ambient_mcp_servers() {
        let args = args_for(ToolPolicy::McpOnly, None);
        assert!(args.iter().any(|a| a == "--strict-mcp-config"));
    }

    /// The restriction must not cost the orchestrator the one server it exists to call.
    #[test]
    fn mcp_only_keeps_the_nucleos_server_reachable() {
        let args = args_for(ToolPolicy::McpOnly, Some(Path::new("C:/tmp/mcp.json")));
        assert!(args.windows(2).any(|w| w[0] == "--mcp-config"));
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--allowedTools" && w[1] == "mcp__nucleos__*")
        );
    }

    #[tokio::test]
    async fn fake_runner_fails_configured_times_then_succeeds() {
        let runner = FakeCommandRunner {
            fail_times: std::sync::Mutex::new(2),
            ..Default::default()
        };

        for _ in 0..2 {
            let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
            assert!(
                runner
                    .run_prompt(
                        "x",
                        &[],
                        None,
                        false,
                        None,
                        None,
                        ToolPolicy::Unrestricted,
                        tx
                    )
                    .await
                    .is_err()
            );
        }
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(
            runner
                .run_prompt(
                    "x",
                    &[],
                    None,
                    false,
                    None,
                    None,
                    ToolPolicy::Unrestricted,
                    tx
                )
                .await
                .is_ok()
        );
        assert_eq!(*runner.calls.lock().unwrap(), 3);
    }
}
