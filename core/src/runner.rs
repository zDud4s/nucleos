use async_trait::async_trait;
use std::path::Path;
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

/// Ollama stays on loopback so local triage cannot accidentally send message bodies off-machine,
/// matching the daemon's own localhost-only transport boundary.
pub const OLLAMA_BASE_URL: &str = "http://127.0.0.1:11434";

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunUsage {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub num_turns: Option<i64>,
}

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
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    pub num_turns: Option<i64>,
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
    /// No tools at all. The run reads its prompt and answers; it cannot reach the filesystem, the
    /// shell, or the network. This is what lets a run process untrusted third-party content at all
    /// (spec §5.5), and it is the CLI's own refusal rather than a hook's cooperation.
    None,
}

fn advertised_tools_violate(policy: ToolPolicy, advertised: &[String]) -> Option<String> {
    let offending: Vec<&str> = match policy {
        ToolPolicy::Unrestricted => return None,
        ToolPolicy::None => advertised.iter().map(String::as_str).collect(),
        ToolPolicy::McpOnly => advertised
            .iter()
            .map(String::as_str)
            .filter(|name| {
                name.strip_prefix("mcp__nucleos__")
                    .is_none_or(|tool| tool.contains("__"))
            })
            .collect(),
    };

    (!offending.is_empty()).then(|| {
        format!(
            "ToolPolicy::{policy:?} violated by CLI-advertised tools: {}",
            offending.join(", ")
        )
    })
}

/// Built-in tool names denied under `ToolPolicy::McpOnly`.
///
/// This list still applies the restriction, but the init-event assertion means it is no longer the
/// only thing standing between a CLI upgrade and a silently wider policy.
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
    match tool_policy {
        ToolPolicy::Unrestricted => {}
        ToolPolicy::McpOnly => {
            // Drops every MCP server this user happens to have configured — the ambient surface a
            // spawned run inherits otherwise includes file-writing connectors.
            args.push("--strict-mcp-config".to_string());
            args.push("--disallowedTools".to_string());
            args.push(BUILTIN_TOOLS.join(","));
        }
        // Measured against CLI 2.1.198: this yields an `init` event advertising NO tools at all —
        // the capability is absent rather than refused, so there is nothing for a prompt injected
        // into a mail body to talk the model into reaching for. `--strict-mcp-config` is redundant
        // under the wildcard and passed anyway, so a future narrowing of one is not a silent
        // widening of the other.
        ToolPolicy::None => {
            args.push("--strict-mcp-config".to_string());
            args.push("--disallowedTools".to_string());
            args.push("*".to_string());
        }
    }
    args
}

/// The final text of a `claude -p --output-format stream-json` run.
///
/// This lives at the núcleo↔CLI boundary because knowing the CLI's output format is this module's
/// job — every caller that needs the answer of a run needs the same parse, and a second copy of it
/// would drift the day the format does.
///
/// Each line is a JSON object; the reply is the last non-empty `result` string. `None` means there
/// was no such event, and callers fall back to the raw stream rather than lose the output.
pub(crate) fn extract_reply(stdout: &str) -> Option<String> {
    let mut reply: Option<String> = None;
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line)
            && v.get("type").and_then(|t| t.as_str()) == Some("result")
            && let Some(text) = v.get("result").and_then(|r| r.as_str())
            && !text.trim().is_empty()
        {
            reply = Some(text.to_string());
        }
    }
    reply
}

/// Usage reported by the final `result` event of a Claude `stream-json` transcript.
///
/// Missing fields stay unknown rather than becoming measured zeroes. `num_turns` belongs to the
/// result event itself; the token counts live under its `usage` object.
pub(crate) fn extract_usage(stdout: &str) -> RunUsage {
    let mut usage = RunUsage::default();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line)
            && value.get("type").and_then(|kind| kind.as_str()) == Some("result")
        {
            usage = RunUsage {
                input_tokens: value
                    .get("usage")
                    .and_then(|result_usage| result_usage.get("input_tokens"))
                    .and_then(serde_json::Value::as_i64),
                output_tokens: value
                    .get("usage")
                    .and_then(|result_usage| result_usage.get("output_tokens"))
                    .and_then(serde_json::Value::as_i64),
                cache_read_tokens: value
                    .get("usage")
                    .and_then(|result_usage| result_usage.get("cache_read_input_tokens"))
                    .and_then(serde_json::Value::as_i64),
                num_turns: value.get("num_turns").and_then(serde_json::Value::as_i64),
            };
        }
    }
    usage
}

/// Kills a spawned CLI's whole process TREE when a run is dropped mid-flight.
///
/// `kill_on_drop` reaches the direct child and stops there, but `claude` is a supervisor: it spawns
/// bash, cargo, git and node to do the actual work. Terminating only the parent orphans those, and
/// an orphaned `cargo build` keeps file locks inside the worktree that the run was supposed to
/// release — which is what makes `git worktree remove` fail through its entire backoff and leaves
/// the GC reporting the same failure every half hour.
///
/// Sound only while the `Child` is still alive, because the open process handle is what stops
/// Windows reusing the pid. Hence `disarm()` the moment the child is reaped, and hence the killer
/// is declared AFTER the child so it drops FIRST.
struct TreeKiller {
    pid: u32,
    armed: bool,
}

impl TreeKiller {
    fn new(pid: u32) -> Self {
        Self { pid, armed: true }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TreeKiller {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // Best effort by definition: this runs while a future is being dropped, so it cannot await
        // and cannot report. `/T` is the whole point (the tree), `/F` because a cancelled run is
        // not being asked politely.
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &self.pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

/// Why an Ollama model cannot safely accept the local triage prompt.
///
/// The distinctions are operational rather than cosmetic: a smaller context needs a different
/// model, an absent model needs installation, and an unreadable response means the probe itself is
/// untrustworthy. Collapsing them would leave startup unable to tell an unsafe configuration from
/// broken infrastructure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelError {
    /// The model exists but Ollama reports a context below the prompt capacity contract.
    ContextTooSmall {
        available_tokens: usize,
        required_tokens: usize,
    },
    /// Ollama explicitly reported that the requested model is unavailable.
    ModelUnavailable(String),
    /// A syntactically valid response omitted the object that describes the model.
    MissingModelInfo,
    /// Model information exists, but no architecture-specific context-length key does.
    MissingContextLength,
    /// Multiple context lengths without a declared architecture are unsafe to resolve by guessing:
    /// choosing the wrong subsystem can make the probe bless a model that truncates the prompt.
    AmbiguousContextLength,
    /// A context-length key exists but does not contain a non-negative integer usable here.
    InvalidContextLength,
    /// The response was empty or was not JSON, so no safety claim can be made from it.
    UnparseableResponse(String),
}

/// Whether startup may expose the configured local runner to triage runs.
///
/// `Disabled` means local triage does not run; it does **not** authorize substituting the remote
/// CLI. Local inference exists so message bodies never leave the machine, and a silent remote
/// fallback would violate that promise precisely when the failed probe is least visible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalTriageDecision {
    Enabled,
    Disabled(String),
}

/// PURE: converts every context-probe outcome into an operator-readable startup decision.
pub fn local_triage_decision(probe: Result<(), ModelError>) -> LocalTriageDecision {
    match probe {
        Ok(()) => LocalTriageDecision::Enabled,
        Err(ModelError::ContextTooSmall {
            available_tokens,
            required_tokens,
        }) => LocalTriageDecision::Disabled(format!(
            "local model context has {available_tokens} tokens but {required_tokens} are required"
        )),
        Err(ModelError::ModelUnavailable(reason)) => {
            LocalTriageDecision::Disabled(if reason.trim().is_empty() {
                "the configured local model is unavailable".to_string()
            } else {
                format!("the configured local model is unavailable: {reason}")
            })
        }
        Err(ModelError::MissingModelInfo) => {
            LocalTriageDecision::Disabled("the local model probe omitted model_info".to_string())
        }
        Err(ModelError::MissingContextLength) => LocalTriageDecision::Disabled(
            "the local model probe omitted its context length".to_string(),
        ),
        Err(ModelError::AmbiguousContextLength) => LocalTriageDecision::Disabled(
            "the local model probe reported ambiguous context lengths".to_string(),
        ),
        Err(ModelError::InvalidContextLength) => LocalTriageDecision::Disabled(
            "the local model probe reported an invalid context length".to_string(),
        ),
        Err(ModelError::UnparseableResponse(reason)) => {
            LocalTriageDecision::Disabled(if reason.trim().is_empty() {
                "the local model probe returned an unreadable response".to_string()
            } else {
                format!("the local model probe returned an unreadable response: {reason}")
            })
        }
    }
}

/// PURE: decides whether an Ollama `/api/show` response proves the model can hold the local prompt.
///
/// Ollama prefixes `context_length` with the model architecture, and multimodal models may report
/// several such keys. The declared architecture is therefore authoritative; without it, only a
/// single unambiguous key can support a safety claim. Every missing or unreadable field fails
/// closed because guessing a limit lets Ollama silently truncate third-party mail before it is
/// classified.
pub fn interpret_context_probe(
    response_json: &str,
    required_tokens: usize,
) -> Result<(), ModelError> {
    if response_json.trim().is_empty() {
        return Err(ModelError::UnparseableResponse(
            "empty response".to_string(),
        ));
    }

    let response: serde_json::Value = serde_json::from_str(response_json)
        .map_err(|error| ModelError::UnparseableResponse(error.to_string()))?;

    if let Some(error) = response.get("error") {
        let reason = error
            .as_str()
            .map(str::to_string)
            .unwrap_or_else(|| error.to_string());
        return Err(ModelError::ModelUnavailable(reason));
    }

    let model_info = response
        .get("model_info")
        .and_then(serde_json::Value::as_object)
        .ok_or(ModelError::MissingModelInfo)?;
    let context_length = match model_info.get("general.architecture") {
        Some(architecture) => {
            let architecture = architecture
                .as_str()
                .ok_or(ModelError::MissingContextLength)?;
            let context_key = format!("{architecture}.context_length");
            model_info
                .get(&context_key)
                .ok_or(ModelError::MissingContextLength)?
        }
        None => {
            let mut context_lengths = model_info
                .iter()
                .filter(|(key, _)| key.ends_with(".context_length"));
            let Some((_, context_length)) = context_lengths.next() else {
                return Err(ModelError::MissingContextLength);
            };
            if context_lengths.next().is_some() {
                return Err(ModelError::AmbiguousContextLength);
            }
            context_length
        }
    };
    let available_tokens = context_length
        .as_u64()
        .and_then(|tokens| usize::try_from(tokens).ok())
        .ok_or(ModelError::InvalidContextLength)?;

    if available_tokens < required_tokens {
        return Err(ModelError::ContextTooSmall {
            available_tokens,
            required_tokens,
        });
    }
    Ok(())
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

/// A tool-free Ollama boundary for local triage.
///
/// This is deliberately a separate `CommandRunner` rather than a mode on `ClaudeCliRunner`: the
/// chat endpoint has no tool or session protocol, and pretending otherwise would make ignored CLI
/// controls look enforced. The retained client also reuses connections across local batches.
pub struct OllamaRunner {
    client: reqwest::Client,
    base_url: String,
    model: String,
}

impl OllamaRunner {
    /// Builds a runner for one Ollama model. Trimming trailing slashes keeps the endpoint stable
    /// when configuration uses either `http://localhost:11434` spelling.
    pub fn new(base_url: String, model: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
        }
    }
}

/// Returns the reason an answer cannot safely enter the content-verdict path.
///
/// Ollama's grammar guarantees JSON shape at sampling time, not useful content. A measured 4B
/// model returned `[]` deterministically for real receipt mail; treating that as success increments
/// per-message content failures and can permanently file valid mail as `failed`.
fn unusable_local_answer(answer: &str) -> Option<String> {
    let value: serde_json::Value = match serde_json::from_str(answer) {
        Ok(value) => value,
        Err(error) => {
            return Some(format!("local triage answer was not valid JSON: {error}"));
        }
    };
    let Some(entries) = value.as_array() else {
        return Some("local triage answer was not a JSON array".to_string());
    };
    if entries.is_empty() {
        return Some("local triage answer contained no verdicts".to_string());
    }
    for (index, entry) in entries.iter().enumerate() {
        let usable_summary = entry
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|summary| !summary.trim().is_empty());
        if !usable_summary {
            return Some(format!(
                "local triage verdict {index} had no usable summary"
            ));
        }
    }
    None
}

#[async_trait]
impl CommandRunner for OllamaRunner {
    async fn run_prompt(
        &self,
        prompt: &str,
        _env: &[(String, String)],
        _cwd: Option<&Path>,
        _plan_only: bool,
        _resume_session_id: Option<&str>,
        _mcp_config: Option<&Path>,
        tool_policy: ToolPolicy,
        _session_tx: UnboundedSender<String>,
    ) -> std::io::Result<RunOutcome> {
        if tool_policy != ToolPolicy::None {
            return Err(std::io::Error::other(
                "Ollama local inference supports only ToolPolicy::None",
            ));
        }

        let message_count = prompt.matches("=== BEGIN MESSAGE id=").count();
        if message_count > crate::triage::LOCAL_BATCH_MAX {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "local triage prompt contains {message_count} messages; at most {} fit the probed context contract",
                    crate::triage::LOCAL_BATCH_MAX
                ),
            ));
        }
        let required_items = message_count.max(1);
        let mut format = serde_json::json!({
            "type": "array",
            "minItems": required_items,
            "items": {
                "type": "object",
                "properties": {
                    "id": {"type": "integer"},
                    "class": {
                        "type": "string",
                        "enum": ["urgent", "action", "info", "noise"]
                    },
                    "summary": {
                        "type": "string",
                        "minLength": 1,
                        "maxLength": 200
                    }
                },
                "required": ["id", "class", "summary"]
            }
        });
        if message_count > 0 {
            format
                .as_object_mut()
                .expect("format is constructed as an object above")
                .insert("maxItems".to_string(), serde_json::json!(message_count));
        }

        let response = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(&serde_json::json!({
                "model": self.model,
                "messages": [{"role": "user", "content": prompt}],
                "stream": false,
                "think": false,
                "options": {
                    "num_ctx": crate::triage::LOCAL_NUM_CTX,
                    "temperature": 0
                },
                "format": format
            }))
            .send()
            .await
            .map_err(std::io::Error::other)?
            .error_for_status()
            .map_err(std::io::Error::other)?
            .json::<serde_json::Value>()
            .await
            .map_err(std::io::Error::other)?;
        let answer = response
            .get("message")
            .and_then(|message| message.get("content"))
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                std::io::Error::other("Ollama response did not contain message.content")
            })?
            .to_string();

        if let Some(stderr) = unusable_local_answer(&answer) {
            return Ok(RunOutcome {
                exit_code: 1,
                stdout: answer,
                stderr,
                session_id: None,
                cost_usd: Some(0.0),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                num_turns: None,
            });
        }

        Ok(RunOutcome {
            exit_code: 0,
            stdout: answer,
            stderr: String::new(),
            session_id: None,
            cost_usd: Some(0.0),
            input_tokens: None,
            output_tokens: None,
            cache_read_tokens: None,
            num_turns: None,
        })
    }
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

        // The ONLY `?` from here on. Everything below turns a failure into a failed RunOutcome
        // instead of an `Err`, because `runs::spawn_run` reads `Err` as "the CLI never ran, so a
        // retry cannot double-apply a mutation". A non-UTF-8 byte on stdout or a broken pipe used
        // to share that type with a spawn failure, so a run that had already worked for minutes
        // inside a worktree — and committed — was re-run from the top.
        let mut child = cmd.spawn()?;

        // Dropped BEFORE `child` (reverse declaration order), which is what keeps this sound: tokio
        // holds the process handle for as long as `child` lives, and Windows will not reuse a pid
        // while a handle to it is open. So an armed killer always names the process we spawned.
        //
        // `kill_on_drop` alone terminates the direct child only. `claude` spawns its own tools —
        // bash, cargo, git, node — and TerminateProcess on the parent orphans every one of them:
        // a cancelled run leaves a `cargo build` holding file locks in the worktree, which is
        // exactly what makes `git worktree remove` fail through its whole backoff afterwards.
        let mut tree_killer = child.id().map(TreeKiller::new);

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
        let mut usage = RunUsage::default();

        let mut post_launch_error: Option<std::io::Error> = None;
        let mut policy_violation: Option<String> = None;

        loop {
            let line = match lines.next_line().await {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(error) => {
                    post_launch_error = Some(error);
                    break;
                }
            };
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
                if v.get("type").and_then(|x| x.as_str()) == Some("system")
                    && v.get("subtype").and_then(|x| x.as_str()) == Some("init")
                {
                    let advertised = v
                        .get("tools")
                        .and_then(serde_json::Value::as_array)
                        .into_iter()
                        .flatten()
                        .filter_map(serde_json::Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>();
                    if let Some(reason) = advertised_tools_violate(tool_policy, &advertised) {
                        policy_violation = Some(reason);
                        break;
                    }
                }
                if v.get("type").and_then(|x| x.as_str()) == Some("result") {
                    usage = extract_usage(&line);
                    if let Some(c) = v
                        .get("total_cost_usd")
                        .or_else(|| v.get("cost_usd"))
                        .and_then(|x| x.as_f64())
                    {
                        cost_usd = Some(c);
                    }
                }
            }
        }

        if policy_violation.is_some() {
            // Kill the whole tree first so terminating the supervisor cannot orphan its tools.
            drop(tree_killer.take());
            let _ = child.start_kill();
        }

        let status = match child.wait().await {
            Ok(status) => Some(status),
            Err(error) => {
                post_launch_error.get_or_insert(error);
                None
            }
        };
        // Reaped, so the process is gone and there is nothing left to kill.
        if let Some(killer) = tree_killer.as_mut() {
            killer.disarm();
        }

        let mut stderr_str = stderr_task.await.unwrap_or_default();
        if let Some(reason) = &policy_violation {
            if !stderr_str.is_empty() && !stderr_str.ends_with('\n') {
                stderr_str.push('\n');
            }
            stderr_str.push_str(&format!("nucleos: {reason}\n"));
        }
        let exit_code = match (&policy_violation, &post_launch_error) {
            (Some(_), _) => -1,
            // A stream that failed mid-run is a failed run, never a zero exit: the transcript is
            // incomplete, so "succeeded" is a claim this cannot make.
            (None, Some(error)) => {
                stderr_str.push_str(&format!("\nnucleos: stream failed after launch: {error}\n"));
                -1
            }
            (None, None) => status.and_then(|status| status.code()).unwrap_or(-1),
        };

        Ok(RunOutcome {
            exit_code,
            stdout: stdout_acc,
            stderr: stderr_str,
            session_id,
            cost_usd,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            num_turns: usage.num_turns,
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
    /// What the CLI was handed in its environment. Recorded because a run with a Bash tool can read
    /// its own environment, so which key lands here is a safety property and not a detail.
    pub last_env: std::sync::Mutex<Option<Vec<(String, String)>>>,
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
        env: &[(String, String)],
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
        // Before the failure injection below: what a run was handed is worth knowing even when the
        // launch is made to fail.
        *self.last_env.lock().unwrap() = Some(env.to_vec());
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
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                num_turns: None,
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
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                num_turns: None,
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

    #[test]
    fn extract_reply_pulls_the_result_text() {
        let stdout = r#"{"type":"system","subtype":"init","session_id":"s"}
{"type":"assistant","message":{"content":[{"type":"text","text":"partial"}]}}
{"type":"result","subtype":"success","result":"Here are your projects: alpha, beta.","total_cost_usd":0.08}"#;

        assert_eq!(
            extract_reply(stdout),
            Some("Here are your projects: alpha, beta.".to_string())
        );
    }

    #[test]
    fn extract_reply_returns_none_without_result_event() {
        let stdout = r#"{"type":"system","subtype":"init","session_id":"s"}
{"type":"assistant","message":{"content":[{"type":"text","text":"partial"}]}}"#;

        assert_eq!(extract_reply(stdout), None);
    }

    #[test]
    fn usage_absent_is_none_not_zero() {
        let stdout = r#"{"type":"system","subtype":"init","session_id":"s"}
{"type":"result","subtype":"success","result":"done","total_cost_usd":0.08}"#;

        let usage = extract_usage(stdout);

        assert_eq!(usage.input_tokens, None);
        assert_eq!(usage.output_tokens, None);
        assert_eq!(usage.cache_read_tokens, None);
        assert_eq!(usage.num_turns, None);
    }

    #[test]
    fn partial_usage_keeps_missing_fields_none() {
        let stdout = r#"{"type":"system","subtype":"init","session_id":"s"}
{"type":"result","subtype":"success","result":"done","total_cost_usd":0.08,"num_turns":12,"usage":{"input_tokens":1000,"output_tokens":500,"num_turns":99}}"#;

        let usage = extract_usage(stdout);

        assert_eq!(usage.input_tokens, Some(1000));
        assert_eq!(usage.output_tokens, Some(500));
        assert_eq!(usage.cache_read_tokens, None);
        assert_eq!(usage.num_turns, Some(12));
    }

    /// The transcript a triage run actually produces: the model's own text arrives in a `content`
    /// array several events BEFORE the `result`, and any parse that takes the first JSON array it
    /// sees would answer with the model's thinking instead of its verdict.
    #[test]
    fn extract_reply_ignores_content_arrays_before_the_result() {
        let stdout = r#"{"type":"system","subtype":"init","session_id":"s","tools":[]}
{"type":"assistant","message":{"content":[{"type":"text","text":"[{\"uid\": 1, \"class\": \"noise\"}]"}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"reconsidering"}]}}
{"type":"result","subtype":"success","result":"[{\"uid\": 1, \"class\": \"urgent\", \"summary\": \"server down\"}]","total_cost_usd":0.02}"#;

        let reply = extract_reply(stdout).expect("the result event carries the verdict");
        assert!(reply.contains("urgent"), "{reply}");
        assert!(!reply.contains("noise"), "the draft must not win: {reply}");
    }

    fn args_for(policy: ToolPolicy, mcp: Option<&Path>) -> Vec<String> {
        cli_args("triage this", "sonnet", false, None, mcp, policy)
    }

    #[test]
    fn a_toolless_policy_that_receives_tools_fails_the_run() {
        let empty = Vec::new();
        assert!(advertised_tools_violate(ToolPolicy::None, &empty).is_none());

        let advertised = vec!["Bash".to_string()];
        assert!(advertised_tools_violate(ToolPolicy::None, &advertised).is_some());
    }

    #[test]
    fn mcp_only_rejects_an_advertised_builtin() {
        let mcp_tools = vec![
            "mcp__nucleos__get_run".to_string(),
            "mcp__nucleos__list_projects".to_string(),
        ];
        assert!(advertised_tools_violate(ToolPolicy::McpOnly, &mcp_tools).is_none());

        let mut with_builtin = mcp_tools;
        with_builtin.push("Bash".to_string());
        assert!(advertised_tools_violate(ToolPolicy::McpOnly, &with_builtin).is_some());
    }

    #[test]
    fn advertised_tool_match_is_segment_not_prefix() {
        let valid = vec!["mcp__nucleos__get_run".to_string()];
        assert!(advertised_tools_violate(ToolPolicy::McpOnly, &valid).is_none());

        let nested_server = vec!["mcp__nucleos__x__evil".to_string()];
        assert!(advertised_tools_violate(ToolPolicy::McpOnly, &nested_server).is_some());
    }

    #[test]
    fn unrestricted_accepts_any_advertised_tool_set() {
        let empty = Vec::new();
        assert!(advertised_tools_violate(ToolPolicy::Unrestricted, &empty).is_none());

        let builtins = vec![
            "Read".to_string(),
            "Bash".to_string(),
            "Write".to_string(),
            "Edit".to_string(),
        ];
        assert!(advertised_tools_violate(ToolPolicy::Unrestricted, &builtins).is_none());
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

    /// Barrier 1 of spec §5.5. Measured against CLI 2.1.198, a wildcard deny yields an `init` event
    /// advertising no tools at all — the capability is absent, not refused, so a prompt injected
    /// into a mail body has nothing to talk the model into reaching for.
    #[test]
    fn the_no_tools_policy_denies_everything() {
        let args = args_for(ToolPolicy::None, None);
        let denied = args
            .windows(2)
            .find(|w| w[0] == "--disallowedTools")
            .map(|w| w[1].clone())
            .expect("the triage policy must deny tools");
        assert_eq!(denied, "*");
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

    async fn ollama_runner_returning(answer: &'static str) -> OllamaRunner {
        let app = axum::Router::new()
            .route(
                "/api/show",
                axum::routing::post(|| async {
                    axum::Json(serde_json::json!({
                        "model_info": {"qwen2.context_length": 8192}
                    }))
                }),
            )
            .fallback(axum::routing::post(move || async move {
                axum::Json(serde_json::json!({
                    "response": answer,
                    "message": {"role": "assistant", "content": answer},
                    "done": true
                }))
            }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let _server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        OllamaRunner::new(format!("http://{address}"), "qwen2".to_string())
    }

    async fn run_local(
        runner: &OllamaRunner,
        tool_policy: ToolPolicy,
    ) -> std::io::Result<RunOutcome> {
        let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel();
        runner
            .run_prompt(
                "triage this message",
                &[],
                None,
                false,
                None,
                None,
                tool_policy,
                session_tx,
            )
            .await
    }

    /// A local inference has no billable session. Leaving the cost unknown is not neutral:
    /// `budget::time_approx` assigns every unknown-cost run a paid sixty-second floor, so free mail
    /// triage would steadily consume the autonomy budget.
    #[tokio::test]
    async fn local_triage_run_reports_zero_cost() {
        let runner =
            ollama_runner_returning(r#"[{"id":1,"class":"info","summary":"monthly report"}]"#)
                .await;

        let outcome = run_local(&runner, ToolPolicy::None).await.unwrap();

        assert_eq!(outcome.cost_usd, Some(0.0));
        assert_eq!(outcome.session_id, None);
    }

    /// The local model boundary has no tool protocol at all. Accepting a wider policy would turn a
    /// caller mistake into an apparently sandboxed triage run whose actual capability was never
    /// enforced.
    #[tokio::test]
    async fn a_local_runner_refuses_a_tool_policy_other_than_none() {
        let runner =
            ollama_runner_returning(r#"[{"id":1,"class":"info","summary":"monthly report"}]"#)
                .await;

        for policy in [ToolPolicy::Unrestricted, ToolPolicy::McpOnly] {
            assert!(
                run_local(&runner, policy).await.is_err(),
                "{policy:?} must be rejected before local inference"
            );
        }
    }

    /// Structurally valid JSON can still contain no usable verdict. Treating that as success sends
    /// every unanswered message through the content-failure counter and can permanently file it as
    /// failed after two model answers that said nothing.
    #[tokio::test]
    async fn an_empty_or_summaryless_answer_is_an_infrastructure_failure() {
        for answer in ["[]", r#"[{"id":1,"class":"info","summary":""}]"#] {
            let runner = ollama_runner_returning(answer).await;
            let outcome = run_local(&runner, ToolPolicy::None).await.unwrap();
            assert_ne!(
                outcome.exit_code, 0,
                "an unusable answer must not look like a completed triage run: {answer}"
            );
        }
    }

    /// Ollama's response grammar cannot make a verdict useful: malformed JSON, the wrong top-level
    /// shape, or a missing usable summary must fail at the runner boundary. Otherwise unanswered
    /// mail is counted as a content failure and can be permanently filed as unreadable.
    #[tokio::test]
    async fn a_verdict_without_a_summary_key_is_unusable() {
        for answer in [
            r#"[{"id":1,"class":"info"}]"#,
            r#"[{"id":1,"class":"info","summary":"   "}]"#,
            r#"{"id":1,"class":"info","summary":"x"}"#,
            "not json at all",
        ] {
            let runner = ollama_runner_returning(answer).await;
            let outcome = run_local(&runner, ToolPolicy::None).await.unwrap();

            assert_ne!(
                outcome.exit_code, 0,
                "an unusable local verdict must not look successful: {answer}"
            );
        }
    }

    /// The negative cases carry the safety property: a missing key, a missing model, or a context
    /// one token too small must all fail closed instead of letting Ollama truncate the worst-case
    /// Portuguese mail batch.
    #[test]
    fn the_context_probe_rejects_a_model_that_cannot_hold_the_worst_case() {
        let required_tokens = 8192;
        for response in [
            r#"{"model_info":{"qwen2.context_length":8191}}"#,
            r#"{"error":"model 'qwen2' not found"}"#,
            r#"{"model_info":{}}"#,
            "",
            "{not-json",
        ] {
            let result: Result<(), ModelError> = interpret_context_probe(response, required_tokens);
            assert!(
                result.is_err(),
                "an unsafe or unreadable probe must fail closed: {response:?}"
            );
        }

        let result: Result<(), ModelError> = interpret_context_probe(
            r#"{"model_info":{"qwen2.context_length":8192}}"#,
            required_tokens,
        );
        assert!(result.is_ok());
    }

    /// Startup must fail closed for every unsafe or unreadable probe result: silently substituting
    /// the remote CLI would leak the message bodies that selecting local triage was meant to keep
    /// on the machine.
    #[test]
    fn a_failed_startup_probe_disables_local_triage() {
        assert!(matches!(
            local_triage_decision(Ok(())),
            LocalTriageDecision::Enabled
        ));

        for error in [
            ModelError::ContextTooSmall {
                available_tokens: 4096,
                required_tokens: 8192,
            },
            ModelError::ModelUnavailable("qwen3.5:4b is not installed".to_string()),
            ModelError::MissingModelInfo,
            ModelError::MissingContextLength,
            ModelError::InvalidContextLength,
            ModelError::UnparseableResponse("invalid JSON".to_string()),
            ModelError::AmbiguousContextLength,
        ] {
            match local_triage_decision(Err(error)) {
                LocalTriageDecision::Disabled(reason) => {
                    assert!(
                        !reason.trim().is_empty(),
                        "a disabled local runner needs an operator-facing reason"
                    );
                }
                LocalTriageDecision::Enabled => {
                    panic!("a failed probe must never enable local triage")
                }
            }
        }

        let LocalTriageDecision::Disabled(reason) =
            local_triage_decision(Err(ModelError::ContextTooSmall {
                available_tokens: 4096,
                required_tokens: 8192,
            }))
        else {
            panic!("an undersized context must disable local triage");
        };
        assert!(reason.contains("4096"), "{reason}");
        assert!(reason.contains("8192"), "{reason}");
    }

    /// Multimodal model metadata can advertise several context lengths. Selecting the first key
    /// lets an unrelated projector either reject a safe model or bless an unsafe one, while
    /// accepting ambiguous metadata makes a safety claim the probe cannot support.
    #[test]
    fn the_probe_picks_the_context_of_the_declared_architecture() {
        let required_tokens = 8192;

        let language_model = interpret_context_probe(
            r#"{"model_info":{"general.architecture":"qwen2","clip.context_length":77,"qwen2.context_length":32768}}"#,
            required_tokens,
        );
        assert!(
            language_model.is_ok(),
            "the declared qwen2 architecture has enough context: {language_model:?}"
        );

        let vision_model = interpret_context_probe(
            r#"{"model_info":{"general.architecture":"clip","clip.context_length":77,"qwen2.context_length":32768}}"#,
            required_tokens,
        );
        assert!(
            vision_model.is_err(),
            "the declared clip architecture is too small"
        );

        let ambiguous = interpret_context_probe(
            r#"{"model_info":{"clip.context_length":77,"qwen2.context_length":32768}}"#,
            required_tokens,
        );
        assert!(
            ambiguous.is_err(),
            "multiple context lengths without an architecture must fail closed"
        );
    }
}
