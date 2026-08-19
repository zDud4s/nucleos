use async_trait::async_trait;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::mpsc::UnboundedSender;

/// Ollama stays on loopback so local triage cannot accidentally send message bodies off-machine,
/// matching the daemon's own localhost-only transport boundary.
pub const OLLAMA_BASE_URL: &str = "http://127.0.0.1:11434";

/// Internal outcome code for a process terminated after its event stream stopped making progress.
///
/// OS process exit codes cannot produce this value, so callers can distinguish a progress deadline
/// from an ordinary CLI failure without treating already-started work as a retryable launch error.
pub const PROGRESS_TIMEOUT_EXIT_CODE: i32 = i32::MIN;

/// Internal outcome code for a process terminated for taking more turns than it was allowed.
///
/// Its own value beside the deadline above, and for the same reason that one exists: the two are
/// different diagnoses. A run that went silent stopped producing; a run that hit this was producing
/// the whole time and getting nowhere, which is the failure a wall clock is worst at catching —
/// a fast model in a tight loop reaches neither the clock nor the job's money check.
pub const TURN_CEILING_EXIT_CODE: i32 = i32::MIN + 1;

/// How many model responses one run may take before the daemon stops it.
///
/// **Generous on purpose, and the number has a basis.** The ablation in `.ai/eval/ABLATION.md`
/// measured 19 real cells of this repository's own work; the largest legitimate run took 94 turns
/// (T3xH1). A ceiling below that would stop work that was going to finish, which is the way a brake
/// like this gets switched off for good. Twice the largest thing ever measured is a limit only a
/// run that is not converging can reach.
///
/// A ceiling, not a target: nothing is expected to approach it, and a run that does is a result
/// worth reading rather than a quota to spend.
pub const DEFAULT_MAX_TURNS: i64 = 200;

/// PURE: how many model responses this stream has carried, folded one line at a time.
///
/// One function for both CLIs. Claude says `assistant` once per completed model message; `codex
/// exec` says `turn.completed`. Neither name appears in the other's stream, so a single fold cannot
/// double-count — and the alternative, a counter per CLI, is how a ceiling ends up enforced on one
/// path and quietly absent on the other, which is worse than no ceiling because somebody will
/// believe it is there.
///
/// Counted from the transcript rather than asked of the CLI: measured against CLI 2.1.198, there is
/// no `--max-turns` flag to delegate this to. `--max-budget-usd` exists and is a different brake —
/// money, which the job already has, rather than motion, which nothing had.
pub(crate) fn turns_from_line(line: &str, current: i64) -> i64 {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
        return current;
    };
    match value.get("type").and_then(serde_json::Value::as_str) {
        Some("assistant") | Some("turn.completed") => current.saturating_add(1),
        _ => current,
    }
}

/// PURE: whether a run has used up the turns it was given.
///
/// `None` is no ceiling and stays no ceiling — every caller that has not chosen one keeps exactly
/// today's behaviour. A ceiling of zero or less is read as no ceiling too, and that is a decision
/// rather than an oversight: a misconfiguration that silently stops every run before its first
/// answer is worse than one that silently disables the brake, because the first looks like the
/// daemon being broken and the second looks like the daemon it already was.
pub(crate) fn over_turn_ceiling(turns: i64, ceiling: Option<i64>) -> bool {
    matches!(ceiling, Some(ceiling) if ceiling > 0 && turns >= ceiling)
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunUsage {
    pub input_tokens: Option<i64>,
    pub output_tokens: Option<i64>,
    pub cache_read_tokens: Option<i64>,
    /// Tokens billed to WRITE the cache, at 1.25x or 2x base input depending on the TTL asked for.
    ///
    /// Separate from `cache_read_tokens` because the two are opposite verdicts about the same run.
    /// Reads mean the prefix was found; writes mean it was paid for so a later run could find it.
    /// A run with neither read nor wrote anything — which is the only shape worth complaining about.
    pub cache_creation_tokens: Option<i64>,
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
    pub cache_creation_tokens: Option<i64>,
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

/// Every caller-chosen input to one runner invocation.
///
/// This deliberately does not implement `Default`. The old positional signature kept one
/// parameter per CLI flag so adding a flag could not silently inherit a default nobody chose; a
/// struct with no `Default` preserves that property by requiring every construction site to name
/// every field.
#[derive(Debug)]
pub struct RunRequest {
    pub prompt: String,
    pub env: Vec<(String, String)>,
    pub cwd: Option<PathBuf>,
    pub plan_only: bool,
    pub resume_session_id: Option<String>,
    pub mcp_config: Option<PathBuf>,
    pub tool_policy: ToolPolicy,
    pub progress_timeout: Option<Duration>,
    /// How many model responses this run may take before the daemon stops it. `None` is no ceiling.
    ///
    /// The brake the daemon did not have. A run had a wall clock and its job had a money ceiling
    /// checked BETWEEN nodes — so a single node looping quickly reached neither: fast turns cost
    /// little each and the clock is generous precisely because real work is slow. This counts the
    /// thing that actually runs away.
    ///
    /// Enforced by counting the transcript, in both runner bodies, because the CLI has no flag for
    /// it (2.1.198).
    pub max_turns: Option<i64>,
    pub session_id: Option<String>,
    pub fork_session: bool,
    pub include_partial_messages: bool,
    /// Whether this run's turns arrive on stdin instead of in its argument vector.
    ///
    /// The opt-in is made once, here, because it decides the shape of the launch and cannot be
    /// changed afterwards: `--input-format stream-json` is what turns the CLI's stdin into a channel
    /// a later turn can arrive on, and a process already spawned without it has nothing listening.
    /// Every path that does not ask keeps today's `-p <prompt>` vector and a closed stdin.
    pub steerable: bool,
    /// Whether the classifier — not the CLI's own allow-list — decides what this run may do.
    ///
    /// The daemon's design has two barriers (spec §5.5): the tool policy decides what tools exist,
    /// and the `PreToolUse` classifier decides which calls go through. The CLI's `permissions.allow`
    /// is a third barrier nobody designed, and left in place it is the binding one — those lists are
    /// written for INTERACTIVE work, where whatever is missing gets approved with a click. An
    /// unattended run has nobody to click, so every call outside the list comes back "requires
    /// approval" and the run burns its turns achieving nothing while still exiting 0.
    ///
    /// Measured 2026-07-30: an autonomous run in this very repository could not execute
    /// `cargo --version`. 28 turns, $1.47, zero files touched.
    ///
    /// Only ever `true` where the classifier is verified present — see
    /// `autopilot::classifier_hook_is_wired`. Opening this barrier without the one that replaces it
    /// leaves a run with nothing governing it at all.
    pub classifier_governs_tools: bool,
    /// Where a steerable run's LATER turns arrive from; the first one is always `prompt`.
    ///
    /// `None` beside `steerable: true` is a real state, not an oversight: the prompt still travels
    /// stdin as a `user` line, and stdin then closes, which is exactly the one-turn run the argv path
    /// performs. What it costs is the ability to say anything more.
    pub messages: Option<tokio::sync::mpsc::UnboundedReceiver<String>>,
    /// Whether this run opts in to the operator's ambient MCP surface.
    ///
    /// `false` everywhere today, and that is the point: the strict default closes the exfiltration
    /// path a hostile mail body could otherwise use to reach a file-writing connector the daemon
    /// never asked for. A run pays for every ambient server in re-sent tool definitions on every
    /// turn, so the cost falls on whoever asks rather than on everyone who did not.
    pub ambient_mcp: bool,
    /// Per-run override of the runner's configured model. `None` keeps it.
    pub model: Option<String>,
    /// Which of this server's tools this run is offered, when it is offered any at all.
    ///
    /// `None` — every caller but one — keeps the wildcard: `--allowedTools mcp__nucleos__*`, the
    /// whole server. `Some(names)` narrows it to those names, prefixed here so the caller states
    /// tool names and not CLI syntax.
    ///
    /// **Economy, not a boundary, and the distinction is worth keeping straight.** What a run may
    /// actually reach is decided by the scope of the key in its environment, in `auth::permits`; a
    /// run handed the wildcard and a narrow key is already safe. What it is not is workable: the
    /// model sees a tool, calls it, takes a 403 and burns its turns achieving nothing while still
    /// exiting 0 — the failure this file documents measuring at $1.47 for zero files touched, by a
    /// different cause.
    ///
    /// Only read when `mcp_config` is `Some`, because that is the only branch that writes
    /// `--allowedTools` at all. A narrowing passed without an MCP config narrows nothing, which is
    /// the harmless direction.
    pub allowed_mcp_tools: Option<&'static [&'static str]>,
}

/// One line of `--input-format stream-json` stdin: a single user turn.
///
/// Measured against CLI 2.1.198, this shape is accepted and the run proceeds — the `init` event fires
/// and the process exits 0. Built through `serde_json` rather than `format!` because a turn is
/// delimited by a newline: a prompt containing one, or a quote, would otherwise arrive as two
/// half-parsed lines instead of the single instruction it is.
pub(crate) fn user_message_line(text: &str) -> String {
    let mut line = serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": text },
    })
    .to_string();
    line.push('\n');
    line
}

fn advertised_tools_from_init(init: &serde_json::Value) -> Option<Vec<String>> {
    init.get("tools")
        .and_then(serde_json::Value::as_array)?
        .iter()
        .map(|tool| tool.as_str().map(str::to_string))
        .collect()
}

fn advertised_tools_violate(policy: ToolPolicy, advertised: Option<&[String]>) -> Option<String> {
    if policy == ToolPolicy::Unrestricted {
        return None;
    }
    let Some(advertised) = advertised else {
        return Some(format!(
            "ToolPolicy::{policy:?} could not be verified: CLI init event did not advertise tools"
        ));
    };

    let offending: Vec<&str> = match policy {
        ToolPolicy::Unrestricted => unreachable!("handled above"),
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

/// Whether a run whose stream is exhausted ever got the chance to verify its tool policy.
///
/// Separate from `advertised_tools_violate` because the two absences are different failures. An
/// `init` that omits `tools` is a CLI that answered the question badly; no `init` at all is a CLI
/// that was never asked, because the assertion only runs inside the `init` branch. Both must fail
/// closed under a restrictive policy: the assertion exists so that a CLI change cannot turn barrier
/// 1 of the triage model into a silent no-op, and renaming or dropping the event is exactly such a
/// change — the one the field-level check above cannot see.
///
/// This does not make the run unsafe on its own. `ToolPolicy::None` is enforced by the CLI's own
/// refusal (see the variant's doc comment), not by this check; what would be lost without it is the
/// evidence that the refusal took effect, which is the whole point of measuring it.
fn policy_unverified_after_stream(policy: ToolPolicy, init_seen: bool) -> Option<String> {
    if policy == ToolPolicy::Unrestricted || init_seen {
        return None;
    }
    Some(format!(
        "ToolPolicy::{policy:?} could not be verified: CLI stream contained no system init event"
    ))
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
///
/// "A CLI upgrade" understates when this has to be revisited. `TaskCreate`, `TaskGet`, `TaskList`
/// and `TaskUpdate` appeared on 2.1.198 — the same version this was measured against — and took
/// every assistant turn down with them, because a name absent here is not denied, so the CLI
/// advertises it and `advertised_tools_violate` kills the run at the `init` event. The tool set can
/// move underneath a version that never changed, which means the version number is not the signal:
/// the stderr line naming the offending tools is.
const BUILTIN_TOOLS: &[&str] = &[
    "Agent",
    "Artifact",
    "AskUserQuestion",
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
    "TaskCreate",
    "TaskGet",
    "TaskList",
    "TaskOutput",
    "TaskStop",
    "TaskUpdate",
    "TodoWrite",
    "ToolSearch",
    "WebFetch",
    "WebSearch",
    "Workflow",
    "Write",
];

/// The full `claude` argument vector for one run. Pure, so the flags that decide what a run can
/// reach are asserted in tests instead of inspected on a live process.
pub(crate) fn cli_args(request: &RunRequest, model: &str) -> Vec<String> {
    // The request wins: the runner is built once at startup, the request is made per run, so the
    // reverse ordering would leave a per-run choice unexpressible.
    let model = request.model.as_deref().unwrap_or(model);
    let mut args = vec!["-p".to_string()];
    // A steerable run's prompt is written to stdin instead. Measured against CLI 2.1.198,
    // `-p <prompt> --input-format stream-json` reads the positional AND waits on stdin, so leaving
    // the prompt here as well would enqueue the same instruction twice.
    if !request.steerable {
        args.push(request.prompt.clone());
    }
    args.push("--model".to_string());
    args.push(model.to_string());
    if let Some(sid) = &request.resume_session_id {
        args.push("--resume".to_string());
        args.push(sid.clone());
    } else if let Some(sid) = &request.session_id {
        args.push("--session-id".to_string());
        args.push(sid.clone());
    }
    if request.fork_session {
        args.push("--fork-session".to_string());
    }
    if request.include_partial_messages {
        args.push("--include-partial-messages".to_string());
    }
    args.push("--output-format".to_string());
    args.push("stream-json".to_string());
    if request.steerable {
        args.push("--input-format".to_string());
        args.push("stream-json".to_string());
    }
    args.push("--verbose".to_string());
    // Immediately after `--verbose`, and before anything conditional: the prompt cache is
    // prefix-matched, so a stable prefix is what lets back-to-back job nodes hit it. A flag whose
    // position moves with the request would push every token behind it out of the match.
    args.push("--exclude-dynamic-system-prompt-sections".to_string());
    // `plan_only` first, and `else`, not a second `if`: a plan-only run must be unable to act no
    // matter what else is true of it, so the two must never both be able to write this flag.
    if request.plan_only {
        args.push("--permission-mode".to_string());
        args.push("plan".to_string());
    } else if request.classifier_governs_tools {
        args.push("--permission-mode".to_string());
        args.push("bypassPermissions".to_string());
    }
    if let Some(path) = &request.mcp_config {
        args.push("--mcp-config".to_string());
        args.push(path.to_string_lossy().into_owned());
        args.push("--allowedTools".to_string());
        args.push(match request.allowed_mcp_tools {
            None => "mcp__nucleos__*".to_string(),
            Some(names) => names
                .iter()
                .map(|name| format!("mcp__nucleos__{name}"))
                .collect::<Vec<_>>()
                .join(","),
        });
    }
    match request.tool_policy {
        // No tool denial — the classifier governs what an autopilot run may call — but the ambient
        // MCP servers are nobody's: each one is re-described in full on every turn, and nothing in
        // the daemon's design calls them. The `--mcp-config` block above still runs, so a run
        // carrying `request.mcp_config` keeps its nucleos server under the strict flag.
        //
        // Only this arm is opt-out-able. `McpOnly` and `None` keep their unconditional strict flag,
        // where it is a safety property rather than an economy.
        ToolPolicy::Unrestricted => {
            if !request.ambient_mcp {
                args.push("--strict-mcp-config".to_string());
            }
        }
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

/// A turn as it stands PART WAY THROUGH: what has been written, and what is being done.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct LiveTurn {
    /// The answer so far. Empty means nothing has been said yet, which on a live turn is not the
    /// same claim as a turn that answered with nothing.
    pub text: String,
    /// The tool being run right now, or `None` when the model is writing rather than acting.
    pub doing: Option<String>,
    /// Every tool the turn ran, oldest first.
    ///
    /// Beside the text and not folded into it: a turn that read four files and ran the tests
    /// answered with more than its last paragraph, and the paragraph on its own reads as an opinion
    /// rather than as work. It is also the only place a turn's actions are visible at all — the
    /// window shows the reply, and nothing else ever said what produced it.
    pub did: Vec<ToolCall>,
}

/// One tool call, as much of it as is worth showing.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ToolCall {
    pub name: String,
    /// The one argument that says what this call was about, or `None` when none of them does.
    ///
    /// A path, a command, a pattern, a URL — deliberately not the whole input. A `Write` carries
    /// the file it is writing, and a chat that printed that argument would print the file.
    pub detail: Option<String>,
}

/// The longest detail kept. A command line can be a heredoc.
const DETAIL_LIMIT: usize = 120;

/// The argument of a tool call worth showing beside its name.
///
/// A fixed list of keys tried in order, rather than "the first string in the object": the input
/// keys belong to the tools, and an unknown tool would otherwise contribute whichever field
/// happened to be ordered first — a different answer between two runs of the same call.
fn detail_of(input: &serde_json::Value) -> Option<String> {
    const KEYS: [&str; 6] = ["file_path", "path", "command", "pattern", "url", "query"];
    let found = KEYS
        .iter()
        .find_map(|key| input.get(key).and_then(|value| value.as_str()))?;
    let trimmed = found.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut detail: String = trimmed.chars().take(DETAIL_LIMIT).collect();
    if trimmed.chars().count() > DETAIL_LIMIT {
        detail.push('…');
    }
    Some(detail)
}

/// Distils a stream still being written into the two things worth showing while it is.
///
/// Beside `extract_reply`, and for the same stated reason: knowing the CLI's output format is this
/// module's job. The alternative was to teach the window these shapes, which would put a format the
/// app does not own — and which changes without asking — into the one place that cannot be tested
/// against the real thing.
///
/// The two sources of the same words are the whole difficulty. With `--include-partial-messages`
/// the text arrives twice: once as `text_delta`s while it is typed, and again in the completed
/// `assistant` message. So the deltas fill a buffer, and a completed message REPLACES that buffer
/// with what it says — which is also what makes this correct when no partials arrive at all.
///
/// Completed messages accumulate rather than replace each other: text, a tool call, then more text
/// is one answer with a gap in it, and keeping only the newest message would silently drop
/// everything the model said before it reached for anything.
pub(crate) fn live_from_stream(stream: &str) -> LiveTurn {
    let mut finished: Vec<String> = Vec::new();
    let mut writing = String::new();
    let mut doing: Option<String> = None;
    let mut did: Vec<ToolCall> = Vec::new();

    for line in stream.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        match value.get("type").and_then(|t| t.as_str()) {
            // Only `text_delta` carries a `text`. A tool call's arguments stream as
            // `input_json_delta` under `partial_json`, and reading that as speech would put a
            // half-written JSON object in the middle of a sentence.
            Some("stream_event") => {
                if let Some(text) = value.pointer("/event/delta/text").and_then(|t| t.as_str()) {
                    writing.push_str(text);
                }
            }
            Some("assistant") => {
                let blocks = value
                    .pointer("/message/content")
                    .and_then(|c| c.as_array())
                    .cloned()
                    .unwrap_or_default();
                let text = blocks
                    .iter()
                    .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("text"))
                    .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join(
                        "

",
                    );
                if !text.trim().is_empty() {
                    finished.push(text);
                }
                for block in blocks
                    .iter()
                    .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("tool_use"))
                {
                    let Some(name) = block.get("name").and_then(|n| n.as_str()) else {
                        continue;
                    };
                    did.push(ToolCall {
                        name: name.to_string(),
                        detail: block.get("input").and_then(detail_of),
                    });
                    doing = Some(name.to_string());
                }
                // The message that just completed is the one those deltas were writing.
                writing.clear();
            }
            // A tool answering is the only thing that ends a tool call. Clearing this anywhere else
            // would show the model as writing while a command is still running.
            Some("user") => {
                let returned = value
                    .pointer("/message/content")
                    .and_then(|c| c.as_array())
                    .is_some_and(|blocks| {
                        blocks.iter().any(|block| {
                            block.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                        })
                    });
                if returned {
                    doing = None;
                }
            }
            _ => {}
        }
    }

    if !writing.trim().is_empty() {
        finished.push(writing);
    }

    LiveTurn {
        text: finished.join(
            "

",
        ),
        doing,
        did,
    }
}

/// Context occupied while a Claude `stream-json` run is still alive.
///
/// This is deliberately separate from `extract_usage`: assistant events describe current context
/// pressure during a run, while the final result describes aggregate usage after it has ended.
pub(crate) fn context_fill_from_line(line: &str, current: Option<i64>) -> Option<i64> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return current;
    };

    if value.get("type").and_then(serde_json::Value::as_str) == Some("assistant") {
        let usage = value
            .get("message")
            .and_then(|message| message.get("usage"));
        let input_tokens = usage
            .and_then(|usage| usage.get("input_tokens"))
            .and_then(serde_json::Value::as_i64);
        let cache_read_tokens = usage
            .and_then(|usage| usage.get("cache_read_input_tokens"))
            .and_then(serde_json::Value::as_i64);

        if let (Some(input_tokens), Some(cache_read_tokens)) = (input_tokens, cache_read_tokens) {
            // Cache-read tokens occupy the context window exactly as fresh input tokens do.
            return input_tokens.checked_add(cache_read_tokens).or(current);
        }
    }

    if current.is_none()
        && value.get("type").and_then(serde_json::Value::as_str) == Some("system")
        && value.get("subtype").and_then(serde_json::Value::as_str) == Some("thinking_tokens")
    {
        return value
            .get("estimated_tokens")
            .and_then(serde_json::Value::as_i64)
            .or(current);
    }

    current
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
                cache_creation_tokens: value
                    .get("usage")
                    .and_then(|result_usage| result_usage.get("cache_creation_input_tokens"))
                    .and_then(serde_json::Value::as_i64),
                num_turns: value.get("num_turns").and_then(serde_json::Value::as_i64),
            };
        }
    }
    usage
}

// The `TreeKiller` this module used to define lives in `process_tree.rs` now, together with the
// spawn contract that makes it correct off Windows. This copy was the one whose `Drop` called
// `taskkill` with no `cfg` at all — a silent no-op on every other platform.
use crate::process_tree::TreeKiller;

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
    /// `RunRequest` intentionally has no `Default`, so adding a CLI flag cannot silently inherit a
    /// value nobody chose. The two parameters outside it are the escape hatches: `session_tx` and
    /// `transcript`, which exist so a caller can learn something before this future resolves.
    ///
    /// `transcript` accumulates stdout as it arrives, and is the ONLY way a caller sees any of it
    /// when the run does not end normally. `runs.rs` bounds the whole call in a wall-clock timeout;
    /// when that fires the future is dropped, and everything owned by it — the returned
    /// `RunOutcome`, its stdout, its usage — is destroyed with it. A run killed by that clock used to
    /// persist no transcript and no trajectory at all, which is precisely the run worth inspecting.
    async fn run_prompt(
        &self,
        request: RunRequest,
        session_tx: UnboundedSender<String>,
        transcript: std::sync::Arc<std::sync::Mutex<String>>,
    ) -> std::io::Result<RunOutcome>;

    /// Runs a prompt while also mirroring the latest known context fill.
    ///
    /// Runners without a streaming context signal keep the default behavior. The CLI runner
    /// overrides this so `runs.rs` can retain the number when its wall clock drops the run future.
    async fn run_prompt_with_context_fill(
        &self,
        request: RunRequest,
        session_tx: UnboundedSender<String>,
        transcript: std::sync::Arc<std::sync::Mutex<String>>,
        _context_fill: std::sync::Arc<std::sync::Mutex<Option<i64>>>,
    ) -> std::io::Result<RunOutcome> {
        self.run_prompt(request, session_tx, transcript).await
    }

    /// The per-role model for a job node's stage, or `None` for the runner's own model.
    ///
    /// Defaulted so a runner with one model — every runner but the CLI one — answers `None` without
    /// having to say so, and so a caller that names no stage keeps today's behavior.
    fn model_for_stage(&self, _stage: Option<&str>) -> Option<String> {
        None
    }
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

/// POSTs one prompt to Ollama's chat endpoint and returns the model's text.
///
/// Lifted out of `OllamaRunner::run_prompt` so a second local-model consumer — the voice pillar's
/// cleanup pass — reaches the same loopback endpoint without standing up a second HTTP client and a
/// second copy of this error handling.
///
/// Two parameters exist because Ollama's payload shape forces them, not for generality's sake:
/// `format` carries a sampling grammar for callers that need one and is OMITTED from the body when
/// `None`, because a grammar is what makes triage's JSON valid by construction and plain-text
/// cleanup must not be constrained by one. `think` is a top-level sibling of `options` rather than a
/// key inside it, so a caller cannot fold it into `options` and have it take effect.
pub async fn ollama_chat(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    prompt: &str,
    options: serde_json::Value,
    format: Option<serde_json::Value>,
    think: bool,
) -> std::io::Result<String> {
    let message = ollama_message(
        client,
        base_url,
        model,
        vec![serde_json::json!({"role": "user", "content": prompt})],
        options,
        format,
        think,
        None,
    )
    .await?;

    message
        .get("content")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| std::io::Error::other("Ollama response did not contain message.content"))
        .map(str::to_string)
}

/// One exchange with the chat endpoint, returning the assistant message whole.
///
/// `ollama_chat` above answers "what did the model say"; this answers "what did the model do",
/// which for a turn with tools is a different question — the reply that matters may carry no text
/// at all and only a `tool_calls` array. Returning the message object rather than its content is
/// what lets a caller tell those apart instead of reading an empty string as an empty answer.
///
/// `tools`, like `format`, is OMITTED from the body when `None` rather than sent as null: a model
/// served a `tools` key it was not meant to see may answer with a tool call nobody can execute.
#[expect(
    clippy::too_many_arguments,
    reason = "the parameters are the fields of Ollama's /api/chat body, one each. A struct would \
              put a second name on a shape the endpoint already defines, and the next field it \
              grows would then have to be added in two places rather than one."
)]
pub async fn ollama_message(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    messages: Vec<serde_json::Value>,
    options: serde_json::Value,
    format: Option<serde_json::Value>,
    think: bool,
    tools: Option<serde_json::Value>,
) -> std::io::Result<serde_json::Value> {
    let mut body = serde_json::json!({
        "model": model,
        "messages": messages,
        "stream": false,
        "think": think,
        "options": options
    });
    let object = body
        .as_object_mut()
        .expect("body is constructed as an object literal above");
    if let Some(format) = format {
        object.insert("format".to_string(), format);
    }
    if let Some(tools) = tools {
        object.insert("tools".to_string(), tools);
    }

    // The error a caller gets back says WHICH failure this was, because two of them mean opposite
    // things to a caller that runs in a loop. A timeout is about this one request and the next may
    // well succeed; a refusal to connect is about the endpoint and every request after it will fail
    // the same way. `pii_shadow`'s sweep reads exactly that distinction to decide between moving
    // past one field and abandoning the pass.
    // One classification, applied to every await that can expire, rather than to the first one. The
    // client's timeout covers the body read as well as the request, and a timeout surfacing there
    // used to be reported as `Other` — which this function's own contract, three lines up, says
    // means the endpoint is gone. The caller would have abandoned its pass over a slow answer.
    fn classify(error: reqwest::Error) -> std::io::Error {
        if error.is_timeout() {
            std::io::Error::new(std::io::ErrorKind::TimedOut, error)
        } else {
            std::io::Error::other(error)
        }
    }

    let response = client
        .post(format!("{base_url}/api/chat"))
        .json(&body)
        .send()
        .await
        .map_err(classify)?;

    // The BODY, not just the status. `error_for_status` throws it away, and it is where Ollama says
    // what was wrong — "this model does not support thinking", say. Without it a configuration
    // mistake is indistinguishable from a network one in the log, and the caller that wants to tell
    // an operator which of the two it is has nothing to read.
    let status = response.status();
    if !status.is_success() {
        let detail = response.text().await.unwrap_or_default();
        return Err(std::io::Error::other(format!(
            "Ollama returned {status}: {}",
            detail.trim()
        )));
    }

    let response = response
        .json::<serde_json::Value>()
        .await
        .map_err(classify)?;

    response
        .get("message")
        .cloned()
        .ok_or_else(|| std::io::Error::other("Ollama response did not contain a message"))
}

/// The loopback chat endpoint, as the one exchange `local_agent`'s loop needs.
///
/// Separate from `OllamaRunner` rather than a method on it, because they are different shapes and
/// the difference matters: a `CommandRunner` answers a prompt and is forbidden tools, while this
/// carries a conversation and exists to offer them. Folding the second into the first would put a
/// tool-bearing path behind a type whose whole documented promise is `ToolPolicy::None`.
pub struct OllamaChat {
    client: reqwest::Client,
    base_url: String,
    model: String,
}

/// Ceiling on one exchange with the local model.
///
/// A turn is up to `MAX_TOOL_ROUNDS` of these, so this is per round rather than per turn — the turn
/// itself is bounded again by the caller. Generous because a cold model loads from disk on the
/// first request, and a first message that times out while Ollama is still starting looks exactly
/// like a broken bot.
const OLLAMA_EXCHANGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

impl OllamaChat {
    pub fn new(base_url: String, model: String) -> Self {
        Self {
            // A client timeout, not a default client. `reqwest::Client::new()` waits for ever, and
            // for ever here means the chat slot is never released and every later message in that
            // chat is refused with 409 until the daemon restarts.
            // `expect` rather than `unwrap_or_default`, which read like a fallback and is not one:
            // `Client::default()` is `Client::new()`, which builds with the same settings and
            // panics on the same failure — so the "fallback" would panic identically, one line
            // later, with a message naming nothing. The causes are TLS backend and proxy
            // environment problems, which are startup misconfiguration; saying so is worth more
            // than pretending to recover.
            client: reqwest::Client::builder()
                .timeout(OLLAMA_EXCHANGE_TIMEOUT)
                .build()
                .expect("HTTP client for the local model (check TLS and proxy environment)"),
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
        }
    }
}

#[async_trait]
impl crate::local_agent::LocalChat for OllamaChat {
    async fn exchange(
        &self,
        messages: Vec<serde_json::Value>,
        tools: Option<Vec<serde_json::Value>>,
    ) -> std::io::Result<serde_json::Value> {
        ollama_message(
            &self.client,
            &self.base_url,
            &self.model,
            messages,
            // No sampling grammar, unlike triage: an answer here is prose for a person, and a
            // grammar is also what would stop the model emitting a tool call at all.
            serde_json::json!({"num_ctx": crate::local_agent::TURN_NUM_CTX}),
            None,
            false,
            tools.map(serde_json::Value::from),
        )
        .await
    }
}

#[async_trait]
impl CommandRunner for OllamaRunner {
    async fn run_prompt(
        &self,
        request: RunRequest,
        _session_tx: UnboundedSender<String>,
        transcript: std::sync::Arc<std::sync::Mutex<String>>,
    ) -> std::io::Result<RunOutcome> {
        if request.tool_policy != ToolPolicy::None {
            return Err(std::io::Error::other(
                "Ollama local inference supports only ToolPolicy::None",
            ));
        }

        let message_count = request.prompt.matches("=== BEGIN MESSAGE id=").count();
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

        // The extracted call, against master's request-struct signature: `prompt` is now
        // `request.prompt`, and the body this builds is pinned by
        // `ollama_request_body_carries_ctx_grammar_and_think` so the wire format could not drift
        // while being moved out of here.
        let answer = ollama_chat(
            &self.client,
            &self.base_url,
            &self.model,
            &request.prompt,
            serde_json::json!({
                "num_ctx": crate::triage::LOCAL_NUM_CTX,
                "temperature": 0
            }),
            Some(format),
            false,
        )
        .await?;

        // One shot, so there is no streaming to mirror — but a caller that reads the transcript
        // after a timeout must not find it empty just because this runner answered all at once.
        if let Ok(mut shared) = transcript.lock() {
            shared.push_str(&answer);
        }

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
                cache_creation_tokens: None,
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
            cache_creation_tokens: None,
            num_turns: None,
        })
    }
}

pub struct ClaudeCliRunner {
    pub model: String,
    /// Where a job's `plan` stage runs, when an operator has named somewhere. `None` keeps `model`.
    pub plan_model: Option<String>,
    /// Where a job's `review` stage runs. `None` keeps `model`.
    pub review_model: Option<String>,
}

#[async_trait]
impl CommandRunner for ClaudeCliRunner {
    /// Only `plan` and `review` are routable: both are read-mostly turns with short output.
    /// `implement` is the turn that writes the code, and answering `Some(_)` for it would move real
    /// work onto whatever a config file happens to name. An unnamed stage is every non-job run.
    fn model_for_stage(&self, stage: Option<&str>) -> Option<String> {
        match stage {
            Some("plan") => self.plan_model.clone(),
            Some("review") => self.review_model.clone(),
            _ => None,
        }
    }

    async fn run_prompt(
        &self,
        request: RunRequest,
        session_tx: UnboundedSender<String>,
        transcript: std::sync::Arc<std::sync::Mutex<String>>,
    ) -> std::io::Result<RunOutcome> {
        self.run_prompt_with_context_fill(
            request,
            session_tx,
            transcript,
            std::sync::Arc::new(std::sync::Mutex::new(None)),
        )
        .await
    }

    async fn run_prompt_with_context_fill(
        &self,
        mut request: RunRequest,
        session_tx: UnboundedSender<String>,
        transcript: std::sync::Arc<std::sync::Mutex<String>>,
        context_fill: std::sync::Arc<std::sync::Mutex<Option<i64>>>,
    ) -> std::io::Result<RunOutcome> {
        // The Claude Code CLI binary. Overridable via `NUCLEOS_CLAUDE_BIN` because on Windows the
        // npm-installed `claude` is a `.cmd` shim that Rust's `Command` can't spawn by name — the
        // daemon points this at the real `claude.exe`. Defaults to `claude` where it's on PATH.
        let claude_bin =
            std::env::var("NUCLEOS_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string());
        let mut cmd = Command::new(&claude_bin);
        cmd.args(cli_args(&request, &self.model));
        for (k, v) in &request.env {
            cmd.env(k, v);
        }
        if let Some(dir) = &request.cwd {
            cmd.current_dir(dir);
        }
        // A closed stdin for every run that did not opt in: there is then no channel for a second
        // author to arrive on, which is the property the email pillar's triage runs depend on.
        if request.steerable {
            cmd.stdin(Stdio::piped());
        } else {
            cmd.stdin(Stdio::null());
        }
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        // kill_on_drop turns an aborted awaiting-task (cancel/timeout, Task 4) into the OS `claude`
        // process actually dying. DO NOT drop this line — Chunk 5 Task 1 adds `--model` and preserves it.
        cmd.kill_on_drop(true);
        // The other half of the `TreeKiller` below: off Windows, killing a tree means killing a
        // process GROUP, and the child has to lead one before it can be named.
        crate::process_tree::spawn_in_own_group(&mut cmd);

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

        // A steerable run's turns are written in their OWN task, concurrently with the stdout loop
        // below, for the same reason stderr is drained in one: a turn written while the CLI is
        // mid-answer must not stop anything reading what it is saying.
        //
        // The task owns the writer, so its end closes the CLI's stdin. That is deliberate and is the
        // whole of the lifetime rule: with no channel it ends after the opening turn, which is the
        // one-turn run the argv path performs; with one it ends when the sender is dropped, or when
        // the abort below reaps it.
        let mut steering_task = None;
        if request.steerable {
            let mut stdin = child
                .stdin
                .take()
                .expect("stdin piped above when steerable");
            let opening = user_message_line(&request.prompt);
            let mut messages = request.messages.take();
            steering_task = Some(tokio::spawn(async move {
                // `ChildStdin` writes straight to the OS pipe, so a completed `write_all` has
                // already been handed over — there is no buffer left to flush. A failed one means
                // the CLI is gone, which the exit code and stderr below already report; there is
                // nothing this task could add and nobody awaiting it to tell.
                if stdin.write_all(opening.as_bytes()).await.is_err() {
                    return;
                }
                let Some(messages) = messages.as_mut() else {
                    return;
                };
                while let Some(text) = messages.recv().await {
                    if stdin
                        .write_all(user_message_line(&text).as_bytes())
                        .await
                        .is_err()
                    {
                        return;
                    }
                }
            }));
        }

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
        let mut session_id = request
            .session_id
            .clone()
            .or_else(|| request.resume_session_id.clone());
        let mut cli_session_seen = false;
        let mut cost_usd: Option<f64> = None;
        let mut usage = RunUsage::default();
        let mut running_context_fill: Option<i64> = None;

        let mut post_launch_error: Option<std::io::Error> = None;
        let mut policy_violation: Option<String> = None;
        let mut init_seen = false;
        let mut progress_timeout_elapsed: Option<Duration> = None;
        let mut turns: i64 = 0;
        let mut turns_exceeded: Option<i64> = None;

        loop {
            let next_line = match request.progress_timeout {
                Some(deadline) => match tokio::time::timeout(deadline, lines.next_line()).await {
                    Ok(result) => result,
                    Err(_) => {
                        progress_timeout_elapsed = Some(deadline);
                        break;
                    }
                },
                None => lines.next_line().await,
            };
            let line = match next_line {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(error) => {
                    post_launch_error = Some(error);
                    break;
                }
            };
            stdout_acc.push_str(&line);
            stdout_acc.push('\n');
            // Mirrored as it arrives, not at the end: the end is exactly what a wall-clock timeout
            // never reaches. A poisoned lock would mean another thread panicked mid-write, which
            // says nothing about this run — keep going rather than take the run down with it.
            if let Ok(mut shared) = transcript.lock() {
                shared.push_str(&line);
                shared.push('\n');
            }
            running_context_fill = context_fill_from_line(&line, running_context_fill);
            if let Ok(mut shared) = context_fill.lock() {
                *shared = running_context_fill;
            }
            // After the line is accumulated and mirrored, never before: a run stopped here still has
            // to leave the transcript of the turn that stopped it, or the evidence for why it was
            // stopped is the one thing missing from the record.
            turns = turns_from_line(&line, turns);
            if over_turn_ceiling(turns, request.max_turns) {
                turns_exceeded = Some(turns);
                break;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                if !cli_session_seen && let Some(sid) = v.get("session_id").and_then(|x| x.as_str())
                {
                    cli_session_seen = true;
                    session_id = Some(sid.to_string());
                    // Best-effort: the receiver may already be gone if the run was cancelled.
                    let _ = session_tx.send(sid.to_string());
                }
                if v.get("type").and_then(|x| x.as_str()) == Some("system")
                    && v.get("subtype").and_then(|x| x.as_str()) == Some("init")
                {
                    init_seen = true;
                    let advertised = advertised_tools_from_init(&v);
                    if let Some(reason) =
                        advertised_tools_violate(request.tool_policy, advertised.as_deref())
                    {
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

        // Only once the stream has ENDED ON ITS OWN can absence of an `init` be read as the CLI
        // never having sent one. A progress timeout or a mid-stream read error means we stopped
        // listening, not that the event was never coming, and asserting otherwise would swap a true
        // diagnosis for a false one.
        if policy_violation.is_none()
            && progress_timeout_elapsed.is_none()
            && post_launch_error.is_none()
        {
            policy_violation = policy_unverified_after_stream(request.tool_policy, init_seen);
        }

        if policy_violation.is_some()
            || progress_timeout_elapsed.is_some()
            || turns_exceeded.is_some()
        {
            // Kill the whole tree first so terminating the supervisor cannot orphan its tools.
            drop(tree_killer.take());
            let _ = child.start_kill();
        }

        // Before `wait()`, and unconditionally: dropping the writer the task owns is the only way a
        // steerable run is told no more turns are coming, and a CLI still listening for one has not
        // finished — so waiting first would be waiting on a process this call is what releases.
        if let Some(steering) = steering_task.take() {
            steering.abort();
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
        if let Some(deadline) = progress_timeout_elapsed {
            if !stderr_str.is_empty() && !stderr_str.ends_with('\n') {
                stderr_str.push('\n');
            }
            stderr_str.push_str(&format!(
                "nucleos: run went silent for {deadline:?}; progress deadline expired\n"
            ));
        }
        if let Some(reached) = turns_exceeded {
            if !stderr_str.is_empty() && !stderr_str.ends_with('\n') {
                stderr_str.push('\n');
            }
            // Says what was reached AND what the limit was. "stopped at 200" alone leaves the reader
            // unable to tell a ceiling that is too low from a run that was never going to finish.
            stderr_str.push_str(&format!(
                "nucleos: stopped after {reached} turns; this run's ceiling was {}\n",
                request.max_turns.unwrap_or_default()
            ));
        }
        let exit_code = match (
            progress_timeout_elapsed,
            turns_exceeded,
            &policy_violation,
            &post_launch_error,
        ) {
            (Some(_), _, _, _) => PROGRESS_TIMEOUT_EXIT_CODE,
            // After the deadline and before the rest: a run killed for looping may well also be a
            // run whose stream then stopped, and the ceiling is the diagnosis that explains the
            // other rather than the other way round.
            (None, Some(_), _, _) => TURN_CEILING_EXIT_CODE,
            (None, None, Some(_), _) => -1,
            // A stream that failed mid-run is a failed run, never a zero exit: the transcript is
            // incomplete, so "succeeded" is a claim this cannot make.
            (None, None, None, Some(error)) => {
                stderr_str.push_str(&format!("\nnucleos: stream failed after launch: {error}\n"));
                -1
            }
            (None, None, None, None) => status.and_then(|status| status.code()).unwrap_or(-1),
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
            cache_creation_tokens: usage.cache_creation_tokens,
            num_turns: usage.num_turns,
        })
    }
}

/// The full `codex exec` argument vector for one run, or the reason this tool cannot perform the
/// run that was asked for. Pure for the same reason `cli_args` is: the flags deciding which model
/// answers and where it is allowed to work are asserted in tests instead of inspected on a live
/// process.
///
/// The `Result` is what differs from `cli_args`. Claude's CLI has a flag for every control
/// `RunRequest` carries, so building that vector cannot fail. `codex exec` has no counterpart for a
/// forked session or for a stdin later turns arrive on, and each of those decides what a run IS
/// rather than how it is decorated — so a vector that quietly dropped one would hand the caller a
/// different run than it asked for (one that loses the history it was meant to branch from, or that
/// answers once and then ignores every steering message) while `runs.rs` recorded it as completed.
/// Naming the field in the refusal is what tells an operator which request cannot take this path.
pub(crate) fn codex_cli_args(request: &RunRequest, model: &str) -> Result<Vec<String>, String> {
    if request.fork_session {
        return Err(
            "codex exec cannot honour fork_session: it has no way to branch an existing session"
                .to_string(),
        );
    }
    if request.steerable {
        return Err(
            "codex exec cannot honour steerable: it has no stdin a later turn can arrive on"
                .to_string(),
        );
    }

    let mut args = vec![
        // The non-interactive subcommand leads the vector; anything else opens a TUI, and a
        // daemon-spawned run has no terminal for one.
        "exec".to_string(),
        // A run works inside a worktree or a plain folder, and the CLI otherwise refuses to start
        // over the shape of that directory — a refusal about the ground rather than about the work.
        "--skip-git-repo-check".to_string(),
        "-m".to_string(),
        model.to_string(),
    ];
    // `-C` is the only thing keeping a run inside the project it was spawned for: the CLI resolves
    // its own project root from this flag, so a vector missing it works wherever the daemon happened
    // to be launched. An absent `cwd` passes no flag rather than inventing a directory.
    if let Some(dir) = &request.cwd {
        args.push("-C".to_string());
        args.push(dir.to_string_lossy().into_owned());
    }
    // Trailing positional, after every flag that takes a value, so a prompt can never be consumed as
    // the argument of the option before it.
    args.push(request.prompt.clone());
    Ok(args)
}

/// Usage reported by a `codex exec` transcript's final `turn.completed` event.
///
/// Absent measurements stay `None` rather than becoming measured zeroes, exactly as `extract_usage`
/// keeps them for the Claude CLI. `budget.rs` bills autonomy against these fields, and this runner
/// exists to be the cheaper path once a spend ceiling has paused the first one — so a `Some(0)`
/// standing in for a tool that said nothing would make every run on this path look free and leave
/// the ceiling with nothing to pause on.
///
/// A plain-text transcript and a structured event carrying no `usage` object are the same silence:
/// unknown in both, and unknown is not zero. `num_turns` and `cache_creation_tokens` have no
/// counterpart in this stream and stay `None` for that reason — counting the events that happened to
/// be read is not the tool reporting a turn count.
pub(crate) fn codex_extract_usage(stdout: &str) -> RunUsage {
    let mut usage = RunUsage::default();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(line)
            && value.get("type").and_then(serde_json::Value::as_str) == Some("turn.completed")
        {
            let reported = value.get("usage");
            usage = RunUsage {
                input_tokens: reported
                    .and_then(|reported| reported.get("input_tokens"))
                    .and_then(serde_json::Value::as_i64),
                output_tokens: reported
                    .and_then(|reported| reported.get("output_tokens"))
                    .and_then(serde_json::Value::as_i64),
                cache_read_tokens: reported
                    .and_then(|reported| reported.get("cached_input_tokens"))
                    .and_then(serde_json::Value::as_i64),
                // Codex reports what it read from the cache and never what it wrote there. Unknown,
                // not zero — and unknown is what disqualifies a Codex run from the cache signal
                // rather than making every one of them look like a miss.
                cache_creation_tokens: None,
                num_turns: None,
            };
        }
    }
    usage
}

/// The second agent CLI, reached only when configuration names it.
///
/// A separate `CommandRunner` rather than a mode on `ClaudeCliRunner`, for the reason `OllamaRunner`
/// is one: the two tools share neither a launch surface nor an output format, and a mode flag would
/// let a control only one of them honours look enforced on both.
pub struct CodexCliRunner {
    pub model: String,
}

#[async_trait]
impl CommandRunner for CodexCliRunner {
    async fn run_prompt(
        &self,
        request: RunRequest,
        _session_tx: UnboundedSender<String>,
        transcript: std::sync::Arc<std::sync::Mutex<String>>,
    ) -> std::io::Result<RunOutcome> {
        // Refused before anything is sent, exactly as `OllamaRunner` refuses a policy it cannot
        // apply. Barrier 1 of the tool model is the CLI's OWN refusal (see `ToolPolicy`), and
        // `codex exec` has neither a flag that denies a tool nor an init event advertising which
        // ones survived — so a run launched here under a restrictive policy would hold every tool
        // while `runs.rs` recorded the restriction as applied.
        if request.tool_policy != ToolPolicy::Unrestricted {
            return Err(std::io::Error::other(format!(
                "codex exec cannot honour ToolPolicy::{:?}: it has no tool-restriction flag",
                request.tool_policy
            )));
        }
        // The same guard, extended to the other controls this launch surface has no counterpart for.
        // Each was accepted and dropped, which is the one outcome a control must never have: the
        // caller is told the run it asked for started, and the record says so too.
        //
        // `plan_only` is the reason this is a refusal rather than a log line. It is how a run is made
        // unable to act — a catch-up run, recovering a schedule the machine slept through, is forced
        // plan-only precisely because nobody chose for it to run NOW. A runner that ignores it turns
        // a deliberately restrained run into an unrestrained one, in the one case where the operator
        // was not watching, and leaves nothing behind that says the restraint was lifted.
        if request.plan_only {
            return Err(std::io::Error::other(
                "codex exec cannot honour plan_only: it has no permission mode that withholds action",
            ));
        }
        // `mcp_config` is half of a pairing: on the Claude path the file arrives with an
        // `--allowedTools mcp__nucleos__*` that narrows the run to that server alone. Dropping the
        // flag drops the narrowing with it, so the run keeps every tool it had — the opposite of what
        // naming an MCP config asks for.
        if request.mcp_config.is_some() {
            return Err(std::io::Error::other(
                "codex exec cannot honour mcp_config: it has no flag that loads one, nor the tool narrowing that comes with it",
            ));
        }
        // Reachable only if the tool-policy guard above is ever loosened, and refused anyway,
        // because of which way it fails. This flag says the daemon has stood the CLI's permission
        // barrier down on the strength of the classifier taking over — and the classifier is a
        // `PreToolUse` hook, which is a Claude Code mechanism `codex exec` knows nothing about.
        // Dropped silently it would not weaken THIS launch, but it would leave the daemon believing
        // a run is governed by something that never ran.
        if request.classifier_governs_tools {
            return Err(std::io::Error::other(
                "codex exec cannot honour classifier_governs_tools: it has no PreToolUse hook, so nothing would replace the barrier this stands down",
            ));
        }
        // Not a safety control, and refused all the same. A caller asks for partial messages because
        // something downstream is waiting on them; a stream that silently never emits any is a
        // feature that looks broken rather than absent.
        if request.include_partial_messages {
            return Err(std::io::Error::other(
                "codex exec cannot honour include_partial_messages: its stream has no partial-message events",
            ));
        }
        // KNOWN LIMITATION, left un-refused on purpose: `resume_session_id` is not honoured here.
        //
        // A run resumed on this path gets a FRESH session carrying the continuation prompt — it
        // re-reads rather than continues — because `codex exec` has no `--resume` flag; resuming is
        // a separate subcommand with its own argument shape, so it is a launch this builder does not
        // yet construct rather than a capability the tool lacks. That is a degraded resume, not an
        // ignored safety control: nothing is loosened by it, and every barrier the run launches
        // under is unchanged.
        //
        // Refusing it would also refuse more than itself. `session_id` — the id the daemon assigns
        // every run so its record has a name — travels the same pair of fields, and the run's outcome
        // is filed under whichever of the two is set; a refusal keyed on either would fail runs whose
        // only unusual property is having been given an identity.
        //
        // The args are built before anything is spawned so a refusal reaches the caller as the `Err` that means
        // the CLI never ran — which `runs::spawn_run` reads as "a retry cannot double-apply a
        // mutation", and that is precisely true of a launch that did not happen.
        let args = codex_cli_args(&request, &self.model).map_err(std::io::Error::other)?;

        // The Codex CLI binary. Overridable via `NUCLEOS_CODEX_BIN` for the same reason
        // `NUCLEOS_CLAUDE_BIN` exists: on Windows the npm-installed `codex` is a `.cmd` shim that
        // Rust's `Command` can't spawn by name, so the daemon points this at the real executable.
        // Defaults to `codex` where it's on PATH.
        let codex_bin = std::env::var("NUCLEOS_CODEX_BIN").unwrap_or_else(|_| "codex".to_string());
        let mut cmd = Command::new(&codex_bin);
        cmd.args(args);
        for (k, v) in &request.env {
            cmd.env(k, v);
        }
        if let Some(dir) = &request.cwd {
            cmd.current_dir(dir);
        }
        // Nothing steerable survives `codex_cli_args`, so stdin is always closed here: there is then
        // no channel for a second author to arrive on.
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        cmd.kill_on_drop(true);
        crate::process_tree::spawn_in_own_group(&mut cmd);

        // The ONLY `?` from here on, for the reason spelled out in `ClaudeCliRunner`: past the spawn
        // a failure becomes a failed `RunOutcome`, because an `Err` claims no work was done.
        let mut child = cmd.spawn()?;

        // Declared after the child so it drops FIRST, which is what keeps the pid it names ours —
        // see `TreeKiller`. `codex` supervises its own tools, so terminating just the parent orphans
        // a build still holding locks inside the worktree the run was supposed to release.
        let mut tree_killer = child.id().map(TreeKiller::new);

        let stdout = child.stdout.take().expect("stdout piped above");
        let stderr = child.stderr.take().expect("stderr piped above");

        // Concurrent, because reading only stdout while the child writes stderr deadlocks the moment
        // the OS stderr pipe buffer fills.
        let stderr_task = tokio::spawn(async move {
            let mut buf = String::new();
            let mut reader = BufReader::new(stderr);
            let _ = reader.read_to_string(&mut buf).await;
            buf
        });

        let mut lines = BufReader::new(stdout).lines();
        let mut stdout_acc = String::new();
        let mut post_launch_error: Option<std::io::Error> = None;
        let mut progress_timeout_elapsed: Option<Duration> = None;
        let mut turns: i64 = 0;
        let mut turns_exceeded: Option<i64> = None;

        loop {
            let next_line = match request.progress_timeout {
                Some(deadline) => match tokio::time::timeout(deadline, lines.next_line()).await {
                    Ok(result) => result,
                    Err(_) => {
                        progress_timeout_elapsed = Some(deadline);
                        break;
                    }
                },
                None => lines.next_line().await,
            };
            let line = match next_line {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(error) => {
                    post_launch_error = Some(error);
                    break;
                }
            };
            stdout_acc.push_str(&line);
            stdout_acc.push('\n');
            // Mirrored as it arrives rather than at the end, because the end is exactly what a
            // wall-clock timeout never reaches, and the transcript is all a dropped run leaves.
            if let Ok(mut shared) = transcript.lock() {
                shared.push_str(&line);
                shared.push('\n');
            }
            // The same brake as the Claude body above, counting `turn.completed` instead of
            // `assistant` — `turns_from_line` knows both, so this path cannot drift out of step
            // with the other by being edited on its own.
            turns = turns_from_line(&line, turns);
            if over_turn_ceiling(turns, request.max_turns) {
                turns_exceeded = Some(turns);
                break;
            }
        }

        if progress_timeout_elapsed.is_some() || turns_exceeded.is_some() {
            // Kill the whole tree first, so terminating the supervisor cannot orphan its tools.
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
        if let Some(deadline) = progress_timeout_elapsed {
            if !stderr_str.is_empty() && !stderr_str.ends_with('\n') {
                stderr_str.push('\n');
            }
            stderr_str.push_str(&format!(
                "nucleos: run went silent for {deadline:?}; progress deadline expired\n"
            ));
        }
        if let Some(reached) = turns_exceeded {
            if !stderr_str.is_empty() && !stderr_str.ends_with('\n') {
                stderr_str.push('\n');
            }
            stderr_str.push_str(&format!(
                "nucleos: stopped after {reached} turns; this run's ceiling was {}\n",
                request.max_turns.unwrap_or_default()
            ));
        }
        let exit_code = match (progress_timeout_elapsed, turns_exceeded, &post_launch_error) {
            (Some(_), _, _) => PROGRESS_TIMEOUT_EXIT_CODE,
            (None, Some(_), _) => TURN_CEILING_EXIT_CODE,
            // A stream that failed mid-run is a failed run, never a zero exit: the transcript is
            // incomplete, so "succeeded" is a claim this cannot make.
            (None, None, Some(error)) => {
                stderr_str.push_str(&format!("\nnucleos: stream failed after launch: {error}\n"));
                -1
            }
            (None, None, None) => status.and_then(|status| status.code()).unwrap_or(-1),
        };

        let usage = codex_extract_usage(&stdout_acc);
        Ok(RunOutcome {
            exit_code,
            stdout: stdout_acc,
            stderr: stderr_str,
            // `codex exec` names no session in what it prints, so a run stays known by the id its
            // caller assigned; inventing one here would file it under an id nothing else holds.
            session_id: request
                .session_id
                .clone()
                .or_else(|| request.resume_session_id.clone()),
            // This tool reports no price. Unknown, not free — `budget.rs` bills against this field,
            // and a `Some(0.0)` would make every run on this path look like it spent nothing.
            cost_usd: None,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_creation_tokens: usage.cache_creation_tokens,
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
    /// The prompt the launch was handed. Recorded because a turn's prompt is not always the text
    /// the person typed — an errand's notebook is prepended to it — so what the CLI actually
    /// received is the only place that injection can be observed.
    pub last_prompt: std::sync::Mutex<Option<String>>,
    pub last_cwd: std::sync::Mutex<Option<std::path::PathBuf>>,
    pub last_resume: std::sync::Mutex<Option<String>>,
    pub last_mcp_config: std::sync::Mutex<Option<std::path::PathBuf>>,
    pub last_tool_policy: std::sync::Mutex<Option<ToolPolicy>>,
    pub last_session_id: std::sync::Mutex<Option<String>>,
    pub last_fork_session: std::sync::Mutex<Option<bool>>,
    pub last_include_partial_messages: std::sync::Mutex<Option<bool>>,
    /// Whether the launch was given a stdin a second author could arrive on. Recorded for the same
    /// reason `last_tool_policy` is: it decides what a run CAN have done to it, so which value
    /// reached the runner is a safety property rather than a detail of the request.
    pub last_steerable: std::sync::Mutex<Option<bool>>,
    /// Whether the launch handed the run the classifier's permission surface instead of the CLI's.
    /// Recorded for the same reason `last_tool_policy` is: it decides what a run CAN do.
    pub last_classifier_governs_tools: std::sync::Mutex<Option<bool>>,
    /// What the CLI was handed in its environment. Recorded because a run with a Bash tool can read
    /// its own environment, so which key lands here is a safety property and not a detail.
    pub last_env: std::sync::Mutex<Option<Vec<(String, String)>>>,
    /// Test-only: return an `Err` (simulated launch failure — no work done) for the first N calls.
    pub fail_times: std::sync::Mutex<u32>,
    /// Test-only: count of run_prompt invocations.
    pub calls: std::sync::Mutex<u32>,
    /// Test-only: the queue a plan node writes, taken by the first call that is given a handoff
    /// directory.
    ///
    /// Written into the directory named by `NUCLEOS_JOB_ARTIFACTS`, exactly where a real plan node
    /// would put it — so a test of the job chain goes through the env plumbing and reads the queue
    /// off disk, instead of reaching around both to seed a queue the daemon never saw. Taken rather
    /// than copied, because only the first node of a job plans: an implement node that rewrote the
    /// queue it is working from is a fiction no real run can produce.
    pub plan_to_write: std::sync::Mutex<Option<String>>,
}

#[cfg(test)]
#[async_trait]
impl CommandRunner for FakeCommandRunner {
    async fn run_prompt(
        &self,
        request: RunRequest,
        session_tx: UnboundedSender<String>,
        transcript: std::sync::Arc<std::sync::Mutex<String>>,
    ) -> std::io::Result<RunOutcome> {
        {
            *self.calls.lock().unwrap() += 1;
        }
        // Before the failure injection below: what a run was handed is worth knowing even when the
        // launch is made to fail.
        *self.last_env.lock().unwrap() = Some(request.env.clone());
        {
            let mut remaining = self.fail_times.lock().unwrap();
            if *remaining > 0 {
                *remaining -= 1;
                return Err(std::io::Error::other("fake launch failure"));
            }
        }
        // Where a plan node's only output goes. Nothing is written unless the caller armed a plan
        // AND the run was handed a handoff directory, so an ordinary run cannot produce one.
        if let Some(artifacts) = request
            .env
            .iter()
            .find(|(key, _)| key == "NUCLEOS_JOB_ARTIFACTS")
            .map(|(_, value)| value)
            && let Some(plan) = self.plan_to_write.lock().unwrap().take()
        {
            let _ = std::fs::create_dir_all(artifacts);
            std::fs::write(std::path::Path::new(artifacts).join("plan.json"), plan)
                .expect("the plan node writes its queue");
        }
        *self.last_prompt.lock().unwrap() = Some(request.prompt.clone());
        *self.last_cwd.lock().unwrap() = request.cwd.clone();
        *self.last_plan_only.lock().unwrap() = Some(request.plan_only);
        *self.last_resume.lock().unwrap() = request.resume_session_id.clone();
        *self.last_mcp_config.lock().unwrap() = request.mcp_config.clone();
        *self.last_tool_policy.lock().unwrap() = Some(request.tool_policy);
        *self.last_session_id.lock().unwrap() = request.session_id.clone();
        *self.last_fork_session.lock().unwrap() = Some(request.fork_session);
        *self.last_include_partial_messages.lock().unwrap() =
            Some(request.include_partial_messages);
        *self.last_steerable.lock().unwrap() = Some(request.steerable);
        *self.last_classifier_governs_tools.lock().unwrap() =
            Some(request.classifier_governs_tools);
        // Clone the canned outcome in its own scope so the MutexGuard drops before any `.await`.
        let mut outcome = {
            let guard = self.canned.lock().unwrap();
            // A `result` event rather than bare text, because callers parse this. `extract_reply`
            // reads the reply out of a completed run's stream, and a default that was not a stream
            // meant every test taking this outcome exercised the no-reply path by accident — which
            // is how "a turn with no result event" stayed indistinguishable from a successful one
            // long enough to publish a CLI hook payload to a Telegram chat.
            guard.clone().unwrap_or(RunOutcome {
                exit_code: 0,
                stdout: r#"{"type":"result","subtype":"success","result":"fake output"}"#.into(),
                stderr: String::new(),
                session_id: Some("fake-session-id".into()),
                cost_usd: Some(0.0),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                num_turns: None,
            })
        };
        if outcome.session_id.is_none() {
            outcome.session_id = request
                .session_id
                .clone()
                .or_else(|| request.resume_session_id.clone());
        }
        // Emit session_id *before* any simulated delay — mirrors the real CLI's early `init` message.
        if let Some(sid) = &outcome.session_id {
            let _ = session_tx.send(sid.clone());
        }
        let delay = *self.delay.lock().unwrap();
        let mut streamed = false;
        match (delay, request.progress_timeout) {
            (Some(delay), Some(deadline)) => {
                streamed = true;
                let mut emitted_stdout = String::new();
                let stdout_lines = outcome
                    .stdout
                    .lines()
                    .map(str::to_owned)
                    .collect::<Vec<_>>();
                for line in stdout_lines {
                    if tokio::time::timeout(deadline, tokio::time::sleep(delay))
                        .await
                        .is_err()
                    {
                        let mut timed_out = outcome;
                        timed_out.exit_code = PROGRESS_TIMEOUT_EXIT_CODE;
                        timed_out.stdout = emitted_stdout;
                        if !timed_out.stderr.is_empty() && !timed_out.stderr.ends_with('\n') {
                            timed_out.stderr.push('\n');
                        }
                        timed_out.stderr.push_str(&format!(
                            "nucleos: run went silent for {deadline:?}; progress deadline expired\n"
                        ));
                        return Ok(timed_out);
                    }
                    emitted_stdout.push_str(&line);
                    emitted_stdout.push('\n');
                    // Same contract as the real runner: what has been emitted is visible to the
                    // caller even if this future never gets to return.
                    if let Ok(mut shared) = transcript.lock() {
                        shared.push_str(&line);
                        shared.push('\n');
                    }
                }
            }
            (Some(delay), None) => tokio::time::sleep(delay).await,
            (None, _) => {}
        }
        if !streamed && let Ok(mut shared) = transcript.lock() {
            shared.push_str(&outcome.stdout);
        }
        Ok(outcome)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A transcript sink for the tests that do not read one. Named rather than inlined so that a
    /// test which DOES care about the transcript is visibly different at the call site.
    fn discard_transcript() -> std::sync::Arc<std::sync::Mutex<String>> {
        std::sync::Arc::new(std::sync::Mutex::new(String::new()))
    }

    #[test]
    fn cli_args_always_assigns_a_session_id() {
        let request = baseline_run_request();

        let args = cli_args(&request, "sonnet");

        assert!(args.windows(2).any(|pair| {
            pair[0] == "--session-id" && pair[1] == "123e4567-e89b-42d3-a456-426614174000"
        }));
    }

    #[test]
    fn cli_args_forks_the_session_only_when_asked() {
        let mut forked = baseline_run_request();
        forked.fork_session = true;
        let forked_args = cli_args(&forked, "sonnet");

        let not_forked = baseline_run_request();
        let not_forked_args = cli_args(&not_forked, "sonnet");

        assert!(forked_args.iter().any(|arg| arg == "--fork-session"));
        assert!(!not_forked_args.iter().any(|arg| arg == "--fork-session"));
    }

    /// The default matters more than the opt-in here: a request that did not ask for this must not
    /// carry the flag, or every run in the daemon quietly gets it.
    #[test]
    fn cli_args_stands_the_cli_permission_barrier_down_only_when_asked() {
        let mut governed = baseline_run_request();
        governed.classifier_governs_tools = true;
        let governed_args = cli_args(&governed, "sonnet");

        let default_args = cli_args(&baseline_run_request(), "sonnet");

        assert!(
            governed_args.windows(2).any(|pair| pair
                == [
                    "--permission-mode".to_string(),
                    "bypassPermissions".to_string()
                ]),
            "a run the classifier governs must say so on the command line: {governed_args:?}"
        );
        assert!(
            !default_args.iter().any(|arg| arg == "--permission-mode"),
            "an ordinary run must carry no permission mode at all: {default_args:?}"
        );
    }

    /// A plan-only run is how a run is made unable to act — a catch-up run recovering a schedule the
    /// machine slept through is forced plan-only precisely because nobody chose for it to run NOW.
    /// If both flags could write `--permission-mode`, the order would decide whether that restraint
    /// survives, and order is not where a safety property belongs.
    #[test]
    fn plan_only_outranks_the_classifier_permission_surface() {
        let mut both = baseline_run_request();
        both.plan_only = true;
        both.classifier_governs_tools = true;
        let args = cli_args(&both, "sonnet");

        assert!(
            args.windows(2)
                .any(|pair| pair == ["--permission-mode".to_string(), "plan".to_string()]),
            "plan must win: {args:?}"
        );
        assert!(
            !args.iter().any(|arg| arg == "bypassPermissions"),
            "a plan-only run must never also be handed the standing-down flag: {args:?}"
        );
        assert_eq!(
            args.iter()
                .filter(|arg| *arg == "--permission-mode")
                .count(),
            1,
            "two permission modes on one command line is the CLI's choice, not ours: {args:?}"
        );
    }

    #[test]
    fn cli_args_streams_partial_messages_only_when_asked() {
        let mut streaming = baseline_run_request();
        streaming.include_partial_messages = true;
        let streaming_args = cli_args(&streaming, "sonnet");

        let not_streaming = baseline_run_request();
        let not_streaming_args = cli_args(&not_streaming, "sonnet");

        assert!(
            streaming_args
                .iter()
                .any(|arg| arg == "--include-partial-messages")
        );
        assert!(
            !not_streaming_args
                .iter()
                .any(|arg| arg == "--include-partial-messages")
        );
    }

    /// The opt-in path. `--input-format stream-json` is what turns the CLI's stdin into a channel a
    /// later turn can arrive on, and the initial prompt has to travel that same channel: measured
    /// against CLI 2.1.198, `-p <prompt> --input-format stream-json` reads the positional AND waits
    /// on stdin, so leaving the prompt in argv would enqueue the same instruction twice.
    // The final assertion is `!args.iter().any(...)`, which clippy would rather see as
    // `!args.contains(...)`. Allowed rather than rewritten: this test is frozen, and an assertion is
    // evidence — rephrasing one to satisfy a style lint edits the record of what was checked, even
    // when the two forms agree.
    #[allow(clippy::manual_contains)]
    #[test]
    fn a_steerable_run_writes_its_prompt_as_a_stream_json_user_line() {
        let mut request = baseline_run_request();
        request.steerable = true;

        let args = cli_args(&request, "sonnet");

        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--input-format" && pair[1] == "stream-json"),
            "a steerable run reads its turns from stdin: {args:?}"
        );
        assert!(
            !args
                .windows(2)
                .any(|pair| pair[0] == "-p" && pair[1] == request.prompt),
            "the prompt belongs on stdin as a user line, not after -p: {args:?}"
        );
        assert!(
            !args.iter().any(|arg| *arg == request.prompt),
            "a steered run's prompt must not reach argv at all: {args:?}"
        );
    }

    /// The regression guard for every path that did not ask to be steerable — the email pillar's
    /// triage runs among them. Whatever the opt-in adds, a run without it keeps today's argument
    /// vector: `-p <prompt>`, no `--input-format`, and therefore a stdin nothing can write to.
    #[test]
    fn a_non_steerable_run_keeps_the_argv_prompt() {
        let request = baseline_run_request();
        assert!(!request.steerable, "the baseline must not opt in");

        let args = cli_args(&request, "sonnet");

        assert_eq!(args.first().map(String::as_str), Some("-p"));
        assert_eq!(
            args.get(1).map(String::as_str),
            Some(request.prompt.as_str()),
            "the prompt must stay the argv positional it is today: {args:?}"
        );
        assert!(
            !args.iter().any(|arg| arg == "--input-format"),
            "a run that did not opt in must keep its stdin closed: {args:?}"
        );
    }

    /// The other half of the opt-in: the argv assertions above prove the prompt LEFT argv, and this
    /// proves what it became. The shape was measured against CLI 2.1.198 by probe; a probe is a
    /// finding until something holds it in place.
    ///
    /// Asserted on a text carrying a newline and a quote because a turn is delimited by a newline:
    /// building this line by hand would split such a prompt into two half-parsed instructions, and
    /// the CLI would act on the first one alone.
    #[test]
    fn a_steering_turn_is_one_json_user_line_whatever_its_text_contains() {
        let text = "stop after this file\nand say \"done\"";

        let line = user_message_line(text);

        assert!(line.ends_with('\n'), "a turn is terminated: {line:?}");
        let body = line.strip_suffix('\n').unwrap();
        assert_eq!(
            body.lines().count(),
            1,
            "one turn must be one line: {line:?}"
        );
        let parsed: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(parsed["type"], "user");
        assert_eq!(parsed["message"]["role"], "user");
        assert_eq!(
            parsed["message"]["content"], text,
            "the text must survive framing intact: {line:?}"
        );
    }

    #[tokio::test]
    async fn a_stream_with_no_init_event_still_reports_the_assigned_session_id() {
        let assigned_session_id = "123e4567-e89b-42d3-a456-426614174000";
        let stdout =
            r#"{"type":"result","subtype":"success","result":"done","total_cost_usd":0.08}"#;
        let runner = FakeCommandRunner {
            canned: std::sync::Mutex::new(Some(RunOutcome {
                exit_code: 0,
                stdout: stdout.to_string(),
                stderr: String::new(),
                session_id: None,
                cost_usd: Some(0.08),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                num_turns: None,
            })),
            ..Default::default()
        };
        let request = baseline_run_request();
        let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel();

        let outcome = runner
            .run_prompt(request, session_tx, discard_transcript())
            .await
            .unwrap();

        assert_eq!(
            outcome.session_id.as_deref(),
            Some(assigned_session_id),
            "the assigned session id must survive a stream with no init event"
        );
    }

    /// The wildcard is what every caller but a department gets, and a department gets exactly its
    /// own list — prefixed here, so callers name tools rather than CLI syntax.
    #[test]
    fn a_narrowed_request_advertises_only_the_tools_it_names() {
        let mut request = baseline_run_request();
        request.mcp_config = Some(PathBuf::from("mcp.json"));

        let wide = cli_args(&request, "claude-sonnet-5");
        let flag = wide.windows(2).find(|w| w[0] == "--allowedTools").unwrap();
        assert_eq!(flag[1], "mcp__nucleos__*");

        request.allowed_mcp_tools = Some(&["list_files", "read_team_file"]);
        let narrow = cli_args(&request, "claude-sonnet-5");
        let flag = narrow
            .windows(2)
            .find(|w| w[0] == "--allowedTools")
            .unwrap();
        assert_eq!(
            flag[1],
            "mcp__nucleos__list_files,mcp__nucleos__read_team_file"
        );
        assert!(
            !narrow.iter().any(|arg| arg == "mcp__nucleos__*"),
            "the wildcard must be replaced, not accompanied — one of the two would win and it \
             would not be obvious which"
        );
    }

    /// Narrowing without an MCP config narrows nothing, which is the harmless direction and worth
    /// pinning: the flag is only ever written inside the `mcp_config` branch.
    #[test]
    fn narrowing_a_request_with_no_mcp_server_adds_no_flag() {
        let mut request = baseline_run_request();
        request.allowed_mcp_tools = Some(&["list_files"]);
        assert!(
            !cli_args(&request, "claude-sonnet-5")
                .iter()
                .any(|a| a == "--allowedTools")
        );
    }

    fn baseline_run_request() -> RunRequest {
        RunRequest {
            prompt: "test prompt".to_string(),
            env: Vec::new(),
            cwd: None,
            plan_only: false,
            resume_session_id: None,
            mcp_config: None,
            tool_policy: ToolPolicy::Unrestricted,
            progress_timeout: None,
            max_turns: None,
            session_id: Some("123e4567-e89b-42d3-a456-426614174000".to_string()),
            fork_session: false,
            include_partial_messages: false,
            steerable: false,
            classifier_governs_tools: false,
            ambient_mcp: false,
            model: None,
            messages: None,
            allowed_mcp_tools: None,
        }
    }

    fn test_run_request(prompt: &str) -> RunRequest {
        RunRequest {
            prompt: prompt.to_string(),
            env: Vec::new(),
            cwd: None,
            plan_only: false,
            resume_session_id: None,
            mcp_config: None,
            tool_policy: ToolPolicy::Unrestricted,
            progress_timeout: None,
            max_turns: None,
            session_id: None,
            fork_session: false,
            include_partial_messages: false,
            steerable: false,
            classifier_governs_tools: false,
            ambient_mcp: false,
            model: None,
            messages: None,
            allowed_mcp_tools: None,
        }
    }

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
                cache_creation_tokens: None,
                num_turns: None,
            })),
            last_plan_only: std::sync::Mutex::new(None),
            ..Default::default()
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let outcome = runner
            .run_prompt(test_run_request("what is 6*7"), tx, discard_transcript())
            .await
            .unwrap();
        assert_eq!(outcome.stdout, "42");
        assert_eq!(outcome.exit_code, 0);
        assert_eq!(outcome.cost_usd, Some(1.5));
    }

    #[tokio::test]
    async fn progress_deadline_holds_while_events_arrive() {
        let progress_timeout = std::time::Duration::from_millis(30);
        let runner = FakeCommandRunner {
            canned: std::sync::Mutex::new(Some(RunOutcome {
                exit_code: 0,
                stdout: [
                    r#"{"type":"system","subtype":"init","session_id":"sess-progress"}"#,
                    r#"{"type":"assistant","message":{"content":[{"type":"text","text":"one"}]}}"#,
                    r#"{"type":"assistant","message":{"content":[{"type":"text","text":"two"}]}}"#,
                    r#"{"type":"assistant","message":{"content":[{"type":"text","text":"three"}]}}"#,
                    r#"{"type":"result","subtype":"success","result":"done"}"#,
                ]
                .join("\n"),
                stderr: String::new(),
                session_id: Some("sess-progress".into()),
                cost_usd: Some(0.0),
                input_tokens: None,
                output_tokens: None,
                cache_read_tokens: None,
                cache_creation_tokens: None,
                num_turns: None,
            })),
            // The fake releases one canned event per interval. The complete run therefore lasts
            // well beyond the progress deadline while every individual quiet gap stays below it.
            delay: std::sync::Mutex::new(Some(std::time::Duration::from_millis(20))),
            ..Default::default()
        };
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        let started = tokio::time::Instant::now();
        let mut request = test_run_request("keep working");
        request.progress_timeout = Some(progress_timeout);

        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            runner.run_prompt(request, tx, discard_transcript()),
        )
        .await
        .expect("events should keep the progress deadline alive")
        .expect("the fake run should complete");

        assert_eq!(outcome.exit_code, 0);
        assert!(
            started.elapsed() >= progress_timeout * 3,
            "the fixture must outlive the progress deadline several times over"
        );
    }

    #[tokio::test]
    async fn fake_runner_emits_session_id_before_returning() {
        let runner = FakeCommandRunner::default();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let handle = tokio::spawn(async move {
            runner
                .run_prompt(test_run_request("hi"), tx, discard_transcript())
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
        let mut request = test_run_request("resume please");
        request.resume_session_id = Some("sess-9".to_string());
        runner
            .run_prompt(request, tx, discard_transcript())
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
        let mut request = test_run_request("use mcp");
        request.mcp_config = Some(std::path::PathBuf::from("C:/tmp/mcp.json"));
        request.tool_policy = ToolPolicy::McpOnly;
        runner
            .run_prompt(request, tx, discard_transcript())
            .await
            .unwrap();
        assert_eq!(
            *runner.last_mcp_config.lock().unwrap(),
            Some(std::path::PathBuf::from("C:/tmp/mcp.json"))
        );
    }

    /// One line of a `--include-partial-messages` stream: a slice of text as it is typed.
    fn delta(text: &str) -> String {
        serde_json::json!({
            "type": "stream_event",
            "event": {"type": "content_block_delta", "index": 0,
                      "delta": {"type": "text_delta", "text": text}}
        })
        .to_string()
    }

    /// A whole assistant message, which is what arrives with or without partials.
    fn message(blocks: serde_json::Value) -> String {
        serde_json::json!({"type": "assistant", "message": {"content": blocks}}).to_string()
    }

    fn said(text: &str) -> serde_json::Value {
        serde_json::json!([{"type": "text", "text": text}])
    }

    #[test]
    fn a_stream_in_flight_reads_back_as_what_has_been_written_so_far() {
        let stream = [delta("está"), delta(" quase")].join(
            "
",
        );

        let live = live_from_stream(&stream);

        assert_eq!(live.text, "está quase");
        assert_eq!(live.doing, None);
    }

    /// The deltas and the completed message describe the SAME words, and a reader that took both
    /// would show every sentence twice — which is what a naive concatenation does, and it looks like
    /// the model stuttering rather than like a parsing bug.
    #[test]
    fn a_completed_message_supersedes_the_deltas_it_was_written_from() {
        let stream = [delta("está"), delta(" quase"), message(said("está quase"))].join(
            "
",
        );

        assert_eq!(live_from_stream(&stream).text, "está quase");
    }

    /// Text, a tool, then more text is ONE answer with a gap in the middle. Keeping only the last
    /// message would throw away everything said before the model reached for anything.
    #[test]
    fn text_written_before_and_after_a_tool_call_is_one_answer() {
        let stream = [
            message(said("deixa ver o ficheiro")),
            message(serde_json::json!([{"type": "tool_use", "name": "Read", "input": {}}])),
            serde_json::json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "content": "ok"}]}
            })
            .to_string(),
            message(said("é o parser de datas")),
        ]
        .join(
            "
",
        );

        let live = live_from_stream(&stream);

        assert_eq!(
            live.text,
            "deixa ver o ficheiro

é o parser de datas"
        );
        assert_eq!(live.doing, None);
    }

    /// What it is doing right now, which is the half a spinner cannot say.
    /// What the turn DID, kept beside what it said. A turn that read four files and ran the tests
    /// answered with more than its last paragraph, and the paragraph alone reads as an opinion.
    #[test]
    fn a_stream_reads_back_the_tools_it_ran_in_the_order_it_ran_them() {
        let stream = [
            message(serde_json::json!([
                {"type": "tool_use", "name": "Read", "input": {"file_path": "core/src/parser.rs"}}
            ])),
            message(serde_json::json!([
                {"type": "tool_use", "name": "Bash", "input": {"command": "cargo test parser"}}
            ])),
        ]
        .join(
            "
",
        );

        let did = live_from_stream(&stream).did;

        assert_eq!(did.len(), 2);
        assert_eq!(did[0].name, "Read");
        assert_eq!(did[0].detail.as_deref(), Some("core/src/parser.rs"));
        assert_eq!(did[1].name, "Bash");
        assert_eq!(did[1].detail.as_deref(), Some("cargo test parser"));
    }

    /// A tool whose arguments this does not recognise is still a tool that ran. Naming it with no
    /// detail says less than the truth; leaving it out says something false.
    #[test]
    fn a_tool_with_no_argument_worth_showing_is_still_recorded() {
        let stream = message(serde_json::json!([
            {"type": "tool_use", "name": "TodoWrite", "input": {"todos": []}}
        ]));

        let did = live_from_stream(&stream).did;

        assert_eq!(did.len(), 1);
        assert_eq!(did[0].name, "TodoWrite");
        assert_eq!(did[0].detail, None);
    }

    #[test]
    fn a_stream_says_which_tool_is_running_until_that_tool_returns() {
        let calling = [
            message(said("deixa ver")),
            message(serde_json::json!([{"type": "tool_use", "name": "Bash", "input": {}}])),
        ]
        .join(
            "
",
        );

        assert_eq!(live_from_stream(&calling).doing.as_deref(), Some("Bash"));

        let returned = [
            calling,
            serde_json::json!({
                "type": "user",
                "message": {"content": [{"type": "tool_result", "content": "ok"}]}
            })
            .to_string(),
        ]
        .join(
            "
",
        );

        assert_eq!(live_from_stream(&returned).doing, None);
    }

    /// A turn that has only just started has said nothing, and that is not the same as a turn that
    /// answered with nothing — the caller is asking about a run still in flight.
    #[test]
    fn a_stream_carrying_only_transport_reads_back_empty() {
        let stream = [
            serde_json::json!({"type": "system", "subtype": "init", "session_id": "s"}).to_string(),
            "not json at all".to_string(),
        ]
        .join(
            "
",
        );

        let live = live_from_stream(&stream);

        assert_eq!(live.text, "");
        assert_eq!(live.doing, None);
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

    #[test]
    fn extract_usage_reads_cache_creation() {
        let stdout = r#"{"type":"system","subtype":"init","session_id":"s"}
{"type":"result","subtype":"success","result":"done","num_turns":3,"usage":{"input_tokens":40,"output_tokens":500,"cache_read_input_tokens":0,"cache_creation_input_tokens":12000}}"#;

        let usage = extract_usage(stdout);

        // The run that pays to fill the cache reads nothing back, and this is the shape that proves
        // the two are different facts: without the second number it is indistinguishable from a run
        // that missed the prefix entirely.
        assert_eq!(usage.cache_read_tokens, Some(0));
        assert_eq!(usage.cache_creation_tokens, Some(12000));
    }

    #[test]
    fn absent_cache_creation_stays_unknown() {
        let stdout = r#"{"type":"system","subtype":"init","session_id":"s"}
{"type":"result","subtype":"success","result":"done","usage":{"input_tokens":1000,"cache_read_input_tokens":9000}}"#;

        let usage = extract_usage(stdout);

        // Not `Some(0)`. A transcript that never mentioned cache creation has not reported writing
        // nothing — it has reported nothing, and a detector must be able to tell those apart.
        assert_eq!(usage.cache_creation_tokens, None);
    }

    /// One fold for both CLIs, because their per-turn events cannot appear in the same stream.
    ///
    /// Claude says `assistant` once per completed model message; `codex exec` says `turn.completed`.
    /// Counting both in one function is what keeps the ceiling from being a Claude-only brake — a
    /// limit that silently does not apply on one of the two paths is worse than no limit, because
    /// somebody will believe it is there.
    #[test]
    fn a_turn_is_counted_once_per_model_response_on_either_cli() {
        let claude = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#;
        let codex = r#"{"type":"turn.completed","usage":{"input_tokens":10}}"#;

        assert_eq!(turns_from_line(claude, 0), 1);
        assert_eq!(turns_from_line(codex, 4), 5);

        // Everything else in either stream is not a turn. `stream_event` in particular arrives by
        // the hundred for a single message — counting it would trip a ceiling of 200 inside one
        // paragraph of the model's first answer.
        for quiet in [
            r#"{"type":"stream_event","event":{"delta":{"text":"tok"}}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result"}]}}"#,
            r#"{"type":"system","subtype":"init","session_id":"s"}"#,
            r#"{"type":"result","subtype":"success","num_turns":9}"#,
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"done"}}"#,
            "not json at all",
            "",
        ] {
            assert_eq!(turns_from_line(quiet, 7), 7, "counted a turn for: {quiet}");
        }
    }

    /// Off by one is the entire bug class of a ceiling, so it is asserted on both sides of the edge.
    #[test]
    fn the_ceiling_trips_at_the_number_it_names_and_never_without_one() {
        assert!(!over_turn_ceiling(199, Some(200)));
        assert!(over_turn_ceiling(200, Some(200)));
        assert!(over_turn_ceiling(201, Some(200)));

        // `None` is no ceiling, and it must stay no ceiling however long the run goes. Every caller
        // that has not chosen a limit keeps exactly today's behaviour.
        assert!(!over_turn_ceiling(1, None));
        assert!(!over_turn_ceiling(1_000_000, None));

        // A ceiling of zero or less would stop a run before its first answer. Refused as "no
        // ceiling" rather than honoured, because a misconfiguration that silently disables every
        // run is worse than one that disables the brake.
        assert!(!over_turn_ceiling(1, Some(0)));
        assert!(!over_turn_ceiling(5, Some(-3)));
    }

    /// A run the daemon stopped has not succeeded, and must not be readable as either of the other
    /// two ways a run can end without doing its work.
    #[test]
    fn a_run_stopped_by_the_ceiling_cannot_be_read_as_a_success_or_as_a_timeout() {
        assert_ne!(TURN_CEILING_EXIT_CODE, 0);
        assert_ne!(TURN_CEILING_EXIT_CODE, -1);
        assert_ne!(TURN_CEILING_EXIT_CODE, PROGRESS_TIMEOUT_EXIT_CODE);
    }

    #[test]
    fn context_fill_reads_usage_from_an_assistant_event() {
        let line = r#"{"type":"assistant","message":{"usage":{"input_tokens":1000,"cache_read_input_tokens":9000}}}"#;

        assert_eq!(
            crate::runner::context_fill_from_line(line, None),
            Some(10_000)
        );
    }

    #[test]
    fn context_fill_falls_back_to_thinking_tokens_when_usage_is_absent() {
        let thinking = r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":125}"#;
        let unrelated = r#"{"type":"assistant","message":{"content":[]}}"#;

        let current = crate::runner::context_fill_from_line(thinking, None);
        assert_eq!(current, Some(125));
        assert_eq!(
            crate::runner::context_fill_from_line(unrelated, current),
            Some(125)
        );
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

    fn args_for(policy: ToolPolicy, mcp: Option<&std::path::Path>) -> Vec<String> {
        let mut request = test_run_request("triage this");
        request.mcp_config = mcp.map(std::path::Path::to_path_buf);
        request.tool_policy = policy;
        cli_args(&request, "sonnet")
    }

    fn advertised_tools_from_event(json: &str) -> Option<Vec<String>> {
        let init = serde_json::from_str(json).expect("test init event must be valid JSON");
        advertised_tools_from_init(&init)
    }

    #[test]
    fn an_init_without_tools_fails_a_toolless_policy_run() {
        let advertised = advertised_tools_from_event(r#"{"type":"system","subtype":"init"}"#);

        assert!(advertised_tools_violate(ToolPolicy::None, advertised.as_deref()).is_some());
    }

    #[test]
    fn an_init_without_tools_fails_an_mcp_only_policy_run() {
        let advertised = advertised_tools_from_event(r#"{"type":"system","subtype":"init"}"#);

        assert!(advertised_tools_violate(ToolPolicy::McpOnly, advertised.as_deref()).is_some());
    }

    #[test]
    fn an_explicitly_empty_tool_advertisement_passes_a_toolless_policy_run() {
        let advertised =
            advertised_tools_from_event(r#"{"type":"system","subtype":"init","tools":[]}"#);

        assert!(advertised_tools_violate(ToolPolicy::None, advertised.as_deref()).is_none());
    }

    /// The hole the field-level check cannot see. `advertised_tools_violate` only ever runs inside
    /// the `init` branch, so a CLI that renamed or dropped the event would skip the assertion
    /// entirely and the run would complete looking verified. That is the same failure the assertion
    /// exists to prevent, one level up.
    #[test]
    fn a_restrictive_policy_that_never_saw_an_init_event_is_unverified() {
        for policy in [ToolPolicy::None, ToolPolicy::McpOnly] {
            assert!(
                policy_unverified_after_stream(policy, false).is_some(),
                "{policy:?} must fail closed when no init event arrived"
            );
        }
    }

    #[test]
    fn an_init_event_that_did_arrive_leaves_the_stream_level_check_silent() {
        for policy in [ToolPolicy::None, ToolPolicy::McpOnly] {
            assert!(policy_unverified_after_stream(policy, true).is_none());
        }
    }

    /// `Unrestricted` asserts nothing about tools, so it has nothing to be unable to verify.
    /// Failing it here would break every ordinary run whose transcript happens to lack an `init`.
    #[test]
    fn unrestricted_needs_no_init_event() {
        assert!(policy_unverified_after_stream(ToolPolicy::Unrestricted, false).is_none());
    }

    #[test]
    fn unrestricted_is_unaffected_by_missing_or_empty_tool_advertisements() {
        let missing = advertised_tools_from_event(r#"{"type":"system","subtype":"init"}"#);
        let empty = advertised_tools_from_event(r#"{"type":"system","subtype":"init","tools":[]}"#);

        assert!(advertised_tools_violate(ToolPolicy::Unrestricted, missing.as_deref()).is_none());
        assert!(advertised_tools_violate(ToolPolicy::Unrestricted, empty.as_deref()).is_none());
    }

    #[test]
    fn a_toolless_policy_that_receives_tools_fails_the_run() {
        let empty = Vec::new();
        assert!(advertised_tools_violate(ToolPolicy::None, Some(&empty)).is_none());

        let advertised = vec!["Bash".to_string()];
        assert!(advertised_tools_violate(ToolPolicy::None, Some(&advertised)).is_some());
    }

    #[test]
    fn mcp_only_rejects_an_advertised_builtin() {
        let mcp_tools = vec![
            "mcp__nucleos__get_run".to_string(),
            "mcp__nucleos__list_projects".to_string(),
        ];
        assert!(advertised_tools_violate(ToolPolicy::McpOnly, Some(&mcp_tools)).is_none());

        let mut with_builtin = mcp_tools;
        with_builtin.push("Bash".to_string());
        assert!(advertised_tools_violate(ToolPolicy::McpOnly, Some(&with_builtin)).is_some());
    }

    #[test]
    fn advertised_tool_match_is_segment_not_prefix() {
        let valid = vec!["mcp__nucleos__get_run".to_string()];
        assert!(advertised_tools_violate(ToolPolicy::McpOnly, Some(&valid)).is_none());

        let nested_server = vec!["mcp__nucleos__x__evil".to_string()];
        assert!(advertised_tools_violate(ToolPolicy::McpOnly, Some(&nested_server)).is_some());
    }

    #[test]
    fn unrestricted_accepts_any_advertised_tool_set() {
        let empty = Vec::new();
        assert!(advertised_tools_violate(ToolPolicy::Unrestricted, Some(&empty)).is_none());

        let builtins = vec![
            "Read".to_string(),
            "Bash".to_string(),
            "Write".to_string(),
            "Edit".to_string(),
        ];
        assert!(advertised_tools_violate(ToolPolicy::Unrestricted, Some(&builtins)).is_none());
    }

    /// An autopilot run keeps the full tool set — the hook and the classifier are what govern it,
    /// and denying tools here would break every real run. What it no longer keeps is the AMBIENT MCP
    /// surface: every server the operator happens to have configured is inherited by a spawned run,
    /// and each one's tool definitions are re-sent in full on every turn. Nothing in the daemon's
    /// design calls those servers, so the run pays for them and gets nothing back.
    ///
    /// The two halves are deliberately different directions: no `--disallowedTools` (the tools this
    /// run may call are the classifier's business), but `--strict-mcp-config` (the servers it
    /// inherits are nobody's).
    #[test]
    fn unrestricted_runs_are_strict_about_mcp_by_default() {
        let args = args_for(ToolPolicy::Unrestricted, None);

        assert!(
            !args.iter().any(|a| a == "--disallowedTools"),
            "an autopilot run keeps its tools; the classifier is what governs them: {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "--strict-mcp-config"),
            "an ambient MCP server nobody asked for is re-described on every turn: {args:?}"
        );
    }

    /// The escape hatch for the run that genuinely needs an operator's own servers. It is an opt-in
    /// on the request rather than a default, because the cost is paid by every run that did not ask.
    /// It must not drag a tool DENIAL in with it: dropping the strict flag widens which servers the
    /// run inherits, and that is the only thing it is allowed to change.
    #[test]
    fn ambient_mcp_opt_in_drops_the_strict_flag() {
        let mut request = baseline_run_request();
        request.ambient_mcp = true;

        let args = cli_args(&request, "sonnet");

        assert!(
            !args.iter().any(|a| a == "--strict-mcp-config"),
            "a run that asked for the ambient servers must be allowed to see them: {args:?}"
        );
        assert!(
            !args.iter().any(|a| a == "--disallowedTools"),
            "the opt-in governs servers, not tools: {args:?}"
        );
    }

    /// The strict default must not cost the orchestrator the one server the daemon passes in. The
    /// same property `mcp_only_keeps_the_nucleos_server_reachable` holds for `McpOnly`, asserted for
    /// `Unrestricted` because that arm now carries the strict flag too — and this is the pairing
    /// that would break silently, taking every `mcp__nucleos__*` call with it.
    #[test]
    fn the_nucleos_mcp_server_survives_the_strict_default() {
        let args = args_for(
            ToolPolicy::Unrestricted,
            Some(std::path::Path::new("C:/tmp/mcp.json")),
        );

        assert!(
            args.windows(2).any(|w| w[0] == "--mcp-config"),
            "the daemon's own server must still be named: {args:?}"
        );
        assert!(
            args.iter().any(|a| a == "--strict-mcp-config"),
            "naming a server is not asking for everyone else's: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "--allowedTools" && w[1] == "mcp__nucleos__*"),
            "the server is reachable only if its tools are granted: {args:?}"
        );
    }

    /// The dynamic sections of the system prompt (the date among them) change between runs, and the
    /// prompt cache is prefix-matched: one moving token near the front misses the cache for every
    /// token behind it. Excluding them is worth nothing unless the flag itself sits at a STABLE
    /// position, which is why the placement is asserted and not just the presence.
    #[test]
    fn every_run_excludes_dynamic_system_prompt_sections() {
        let args = cli_args(&baseline_run_request(), "sonnet");

        assert!(
            args.iter()
                .any(|a| a == "--exclude-dynamic-system-prompt-sections"),
            "a system prompt that changes every run cannot be cached: {args:?}"
        );
        assert!(
            args.windows(2).any(|pair| pair[0] == "--verbose"
                && pair[1] == "--exclude-dynamic-system-prompt-sections"),
            "the flag must keep a fixed place in the vector, or the prefix it protects moves: \
             {args:?}"
        );
    }

    /// A per-request model is what lets one job run its plan turn on a larger model than its
    /// implement turns. The request wins over the runner's configured default because the runner is
    /// built once at startup and the request is made per run; the reverse ordering would make the
    /// per-run choice unexpressible.
    #[test]
    fn cli_args_prefer_the_request_model_over_the_runner_default() {
        let mut pinned = baseline_run_request();
        pinned.model = Some("claude-haiku-4-5".to_string());
        let pinned_args = cli_args(&pinned, "sonnet");

        assert!(
            pinned_args
                .windows(2)
                .any(|pair| pair[0] == "--model" && pair[1] == "claude-haiku-4-5"),
            "the request's model must reach the command line: {pinned_args:?}"
        );
        assert!(
            !pinned_args.iter().any(|a| a == "sonnet"),
            "the runner default must not survive alongside it: {pinned_args:?}"
        );

        let unpinned = baseline_run_request();
        assert!(unpinned.model.is_none(), "the baseline must pin nothing");
        let unpinned_args = cli_args(&unpinned, "sonnet");

        assert!(
            unpinned_args
                .windows(2)
                .any(|pair| pair[0] == "--model" && pair[1] == "sonnet"),
            "a request that pins nothing keeps today's model: {unpinned_args:?}"
        );
    }

    /// Which stages may be routed elsewhere, and — the half that matters — which may not. `plan` and
    /// `review` are read-mostly turns whose output is short; `implement` is the turn that writes the
    /// code, and answering `Some(_)` for it would silently move real work onto whatever model the
    /// file happens to name. `None` for an unnamed stage keeps every existing caller on the runner's
    /// own model.
    #[test]
    fn plan_and_review_stages_get_their_configured_models() {
        let configured = ClaudeCliRunner {
            model: "claude-sonnet-5".to_string(),
            plan_model: Some("claude-opus-4-8".to_string()),
            review_model: Some("claude-haiku-4-5".to_string()),
        };

        assert_eq!(
            configured.model_for_stage(Some("plan")).as_deref(),
            Some("claude-opus-4-8")
        );
        assert_eq!(
            configured.model_for_stage(Some("review")).as_deref(),
            Some("claude-haiku-4-5")
        );
        assert_eq!(
            configured.model_for_stage(Some("implement")),
            None,
            "the turn that writes code is not routable by this file"
        );
        assert_eq!(
            configured.model_for_stage(None),
            None,
            "a run that named no stage keeps the runner's own model"
        );

        let unconfigured = ClaudeCliRunner {
            model: "claude-sonnet-5".to_string(),
            plan_model: None,
            review_model: None,
        };
        for stage in [Some("plan"), Some("review"), Some("implement"), None] {
            assert_eq!(
                unconfigured.model_for_stage(stage),
                None,
                "an unconfigured runner routes nothing anywhere: {stage:?}"
            );
        }
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
            "WebSearch",
            "PowerShell",
        ] {
            assert!(
                denied.split(',').any(|t| t == tool),
                "{tool} must be denied under McpOnly"
            );
        }
    }

    /// `BUILTIN_TOOLS` is a DENYLIST, and the direction is the whole point of this test.
    ///
    /// THREAT_MODEL known gap 7 describes the CLI's own web tools as reachable by cron, repo and
    /// manual runs, and says removing them from this list would "unify the path". Read literally
    /// that is backwards in both halves, and acting on it would be a security regression:
    ///
    /// - Those runs reach the web because `ToolPolicy::Unrestricted` pushes **no tool denial at
    ///   all** — asserted next door in `unrestricted_runs_are_strict_about_mcp_by_default`, which
    ///   also pins the one flag that arm DOES push, and it governs MCP servers rather than tools.
    ///   Their presence in this list has nothing to do with it.
    /// - What this list actually does is DENY them to `McpOnly`, which is what every assistant turn
    ///   runs under. Removing a name from here GRANTS that tool to the surface that reads
    ///   summaries of mail written by strangers.
    ///
    /// Closing the real gap means adding `--disallowedTools WebFetch,WebSearch` to the
    /// `Unrestricted` arm, which is a different edit in a different place. This test exists so that
    /// somebody who reaches for the sentence in the threat model instead meets a red test first.
    #[test]
    fn removing_a_web_tool_from_the_denylist_widens_the_assistant_rather_than_narrowing_a_run() {
        // What the denylist governs: the assistant's surface, and nothing else.
        let mcp_only = args_for(ToolPolicy::McpOnly, None);
        let denied = mcp_only
            .windows(2)
            .find(|w| w[0] == "--disallowedTools")
            .map(|w| w[1].clone())
            .expect("McpOnly must deny built-ins");
        for tool in ["WebFetch", "WebSearch"] {
            assert!(
                denied.split(',').any(|t| t == tool),
                "{tool} must stay denied to assistant turns; deleting it from BUILTIN_TOOLS grants \
                 it to the one surface that reads a stranger's words"
            );
        }

        // And what it does NOT govern: an autonomous run's web access, which no flag here touches.
        // If this ever stops holding, the gap closed somewhere else and the threat model's known
        // gap 7 needs rewriting rather than this test relaxing.
        let unrestricted = args_for(ToolPolicy::Unrestricted, None);
        assert!(
            !unrestricted.iter().any(|a| a == "--disallowedTools"),
            "an Unrestricted run carries no denial, so BUILTIN_TOOLS cannot be what governs it"
        );
    }

    /// The regression that took every assistant turn down. The CLI grew a task-management family
    /// on a version this list had already been measured against; the four names were not denied, so
    /// they were advertised, so `advertised_tools_violate` killed each turn at its `init` event.
    ///
    /// Named one by one rather than by prefix: `Task` was in the list and its relatives were not,
    /// which is exactly the gap a prefix check would paper over. `AskUserQuestion` rides along for
    /// the same reason — a built-in that was never on the list, waiting to break the next turn.
    #[test]
    fn mcp_only_denies_the_task_family_and_the_question_tool() {
        let args = args_for(ToolPolicy::McpOnly, None);
        let denied = args
            .windows(2)
            .find(|w| w[0] == "--disallowedTools")
            .map(|w| w[1].clone())
            .expect("McpOnly must deny built-ins");
        for tool in [
            "TaskCreate",
            "TaskGet",
            "TaskList",
            "TaskUpdate",
            "AskUserQuestion",
        ] {
            assert!(
                denied.split(',').any(|t| t == tool),
                "{tool} must be denied under McpOnly, or it is advertised and kills the turn"
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
        let args = args_for(
            ToolPolicy::McpOnly,
            Some(std::path::Path::new("C:/tmp/mcp.json")),
        );
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
                    .run_prompt(test_run_request("x"), tx, discard_transcript())
                    .await
                    .is_err()
            );
        }
        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();
        assert!(
            runner
                .run_prompt(test_run_request("x"), tx, discard_transcript())
                .await
                .is_ok()
        );
        assert_eq!(*runner.calls.lock().unwrap(), 3);
    }

    type SeenBodies = std::sync::Arc<std::sync::Mutex<Vec<serde_json::Value>>>;

    /// Like `ollama_runner_returning`, but keeps every JSON body posted to the chat endpoint.
    ///
    /// The stub used to answer with a canned reply and never look at the request, which meant every
    /// assertion here was about the OUTCOME. An edit that dropped `num_ctx`, dropped the sampling
    /// grammar, or flipped `think` would have left all of them green — so the wire format needs a test
    /// that reads the wire.
    async fn ollama_runner_capturing(answer: &'static str) -> (OllamaRunner, SeenBodies) {
        let seen: SeenBodies = SeenBodies::default();
        let recorder = seen.clone();
        let app = axum::Router::new()
            .route(
                "/api/show",
                axum::routing::post(|| async {
                    axum::Json(serde_json::json!({
                        "model_info": {"qwen2.context_length": 8192}
                    }))
                }),
            )
            .fallback(axum::routing::post(
                move |axum::Json(body): axum::Json<serde_json::Value>| {
                    let recorder = recorder.clone();
                    async move {
                        recorder.lock().unwrap().push(body);
                        axum::Json(serde_json::json!({
                            "response": answer,
                            "message": {"role": "assistant", "content": answer},
                            "done": true
                        }))
                    }
                },
            ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let _server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        (
            OllamaRunner::new(format!("http://{address}"), "qwen2".to_string()),
            seen,
        )
    }

    async fn ollama_runner_returning(answer: &'static str) -> OllamaRunner {
        ollama_runner_capturing(answer).await.0
    }

    async fn run_local(
        runner: &OllamaRunner,
        tool_policy: ToolPolicy,
    ) -> std::io::Result<RunOutcome> {
        run_local_prompt(runner, tool_policy, "triage this message").await
    }

    async fn run_local_prompt(
        runner: &OllamaRunner,
        tool_policy: ToolPolicy,
        prompt: &str,
    ) -> std::io::Result<RunOutcome> {
        let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel();
        // `prompt`, not a fixed string: this helper exists so the batch-bounds and
        // context-contract tests can vary what is sent, and hardcoding it would make both of them
        // assert against a prompt they did not choose.
        let mut request = test_run_request(prompt);
        request.tool_policy = tool_policy;
        runner
            .run_prompt(request, session_tx, discard_transcript())
            .await
    }

    /// Guards the `ollama_chat` extraction, and it has to read the request because every other test
    /// here reads only the outcome.
    ///
    /// Each field asserted is one whose loss is invisible downstream: without `num_ctx` Ollama
    /// truncates the prompt in silence and mail gets filed as unreadable; without the grammar a
    /// malformed answer reaches `parse_verdict`; with `think` on, reasoning tokens land in the
    /// transcript. A refactor that dropped any of them would otherwise ship green.
    #[tokio::test]
    async fn ollama_request_body_carries_ctx_grammar_and_think() {
        let (runner, seen) =
            ollama_runner_capturing(r#"[{"id":1,"class":"info","summary":"monthly report"}]"#)
                .await;

        run_local(&runner, ToolPolicy::None).await.unwrap();

        let body = seen
            .lock()
            .unwrap()
            .first()
            .cloned()
            .expect("run_prompt posted a chat request");
        assert_eq!(
            body["options"]["num_ctx"].as_u64(),
            Some(crate::triage::LOCAL_NUM_CTX as u64)
        );
        assert_eq!(body["options"]["temperature"].as_u64(), Some(0));
        assert_eq!(body["think"].as_bool(), Some(false));
        assert_eq!(body["stream"].as_bool(), Some(false));
        assert_eq!(body["format"]["type"].as_str(), Some("array"));
        assert_eq!(
            body["format"]["items"]["required"],
            serde_json::json!(["id", "class", "summary"])
        );
    }

    /// The batch-size half of the same contract, on a prompt that actually carries message markers.
    ///
    /// Worth its own test because the fixed `"triage this message"` prompt every other case uses
    /// contains none, so `message_count` is zero and the `maxItems` branch — the one that stops the
    /// model returning verdicts for messages that were not in the batch — never executes.
    #[tokio::test]
    async fn ollama_request_body_bounds_items_to_the_batch() {
        let (runner, seen) =
            ollama_runner_capturing(r#"[{"id":1,"class":"info","summary":"monthly report"}]"#)
                .await;

        let prompt = "=== BEGIN MESSAGE id=1 ===\nfirst\n=== BEGIN MESSAGE id=2 ===\nsecond";
        run_local_prompt(&runner, ToolPolicy::None, prompt)
            .await
            .unwrap();

        let body = seen
            .lock()
            .unwrap()
            .first()
            .cloned()
            .expect("run_prompt posted a chat request");
        assert_eq!(body["format"]["minItems"].as_u64(), Some(2));
        assert_eq!(body["format"]["maxItems"].as_u64(), Some(2));
    }

    /// A batch bigger than the probed context contract is a programming error, not a case to handle:
    /// the alternative is letting Ollama truncate the tail and filing the messages it never saw as
    /// unreadable.
    #[tokio::test]
    async fn a_batch_beyond_the_context_contract_is_refused() {
        let (runner, _seen) = ollama_runner_capturing("[]").await;

        let prompt = (0..=crate::triage::LOCAL_BATCH_MAX)
            .map(|id| format!("=== BEGIN MESSAGE id={id} ===\nbody"))
            .collect::<Vec<_>>()
            .join("\n");
        let error = run_local_prompt(&runner, ToolPolicy::None, &prompt)
            .await
            .expect_err("a prompt past LOCAL_BATCH_MAX must not reach the model");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
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

    /// The `codex exec` launch surface, asserted the way `cli_args` is: purely, so the flags that
    /// decide which model answers and where it is allowed to work are checked here rather than
    /// inspected on a live process.
    ///
    /// `--skip-git-repo-check` is what lets a run start at all in a directory the CLI would
    /// otherwise refuse, and `-C` is the only thing keeping a run inside the project it was spawned
    /// for — a vector missing it runs wherever the daemon happens to have been launched.
    #[test]
    fn codex_cli_args_carry_the_model_and_the_working_directory() {
        let mut request = baseline_run_request();
        request.cwd = Some(std::path::PathBuf::from("C:/work/repo"));

        let args = codex_cli_args(&request, "gpt-5.6-terra")
            .expect("a baseline request asks for nothing the tool cannot honour");

        assert_eq!(
            args.first().map(String::as_str),
            Some("exec"),
            "the non-interactive subcommand leads the vector: {args:?}"
        );
        assert!(
            args.iter().any(|arg| arg == "--skip-git-repo-check"),
            "a run must not be refused for the shape of the directory it works in: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "-m" && pair[1] == "gpt-5.6-terra"),
            "the model must immediately follow -m: {args:?}"
        );
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "-C" && pair[1] == "C:/work/repo"),
            "the working directory must immediately follow -C: {args:?}"
        );

        let directoryless = baseline_run_request();
        assert!(
            directoryless.cwd.is_none(),
            "the baseline must name no directory"
        );
        let directoryless_args = codex_cli_args(&directoryless, "gpt-5.6-terra")
            .expect("a request without a directory is still honourable");
        assert!(
            !directoryless_args.iter().any(|arg| arg == "-C"),
            "an absent cwd must not invent a directory: {directoryless_args:?}"
        );
    }

    /// A control this tool cannot honour must fail the launch instead of vanishing from it.
    ///
    /// `fork_session` and `steerable` each change what a run IS, not how it is decorated: a fork
    /// continues someone else's session rather than starting a new one, and a steerable run promises
    /// a stdin that later turns can arrive on. Building a vector that silently omits either hands the
    /// caller a different run than the one it asked for — a run that answers once and then ignores
    /// every steering message, or one that loses the history it was supposed to branch from — and
    /// `runs.rs` records that as a completed run. The refusal names the flag so an operator reading
    /// the failure learns which request cannot take this cheaper path.
    #[test]
    fn codex_cli_args_refuse_what_the_tool_cannot_honour() {
        let honourable = baseline_run_request();
        assert!(
            codex_cli_args(&honourable, "gpt-5.6-terra").is_ok(),
            "the control case must build, or a refusal proves nothing"
        );

        let mut forked = baseline_run_request();
        forked.fork_session = true;
        let forked_refusal = codex_cli_args(&forked, "gpt-5.6-terra")
            .expect_err("a forked session cannot be honoured here");
        assert!(
            forked_refusal.contains("fork_session"),
            "the refusal must name what it could not honour: {forked_refusal}"
        );

        let mut steerable = baseline_run_request();
        steerable.steerable = true;
        let steerable_refusal = codex_cli_args(&steerable, "gpt-5.6-terra")
            .expect_err("a steerable run cannot be honoured here");
        assert!(
            steerable_refusal.contains("steerable"),
            "the refusal must name what it could not honour: {steerable_refusal}"
        );
    }

    /// Barrier 1 of the tool model is the CLI's OWN refusal, and `codex exec` cannot perform it: it
    /// has no flag that denies a tool and no init event advertising which ones survived, so a
    /// restrictive policy could be neither applied at launch nor verified from the stream. A run
    /// started anyway would hold every tool while `runs.rs` recorded the restriction as honoured —
    /// and `ToolPolicy::None` is what lets a run read a stranger's mail at all.
    ///
    /// Asserted against `run_prompt` rather than `codex_cli_args`, because the refusal lives in the
    /// runner and the arg builder never sees the policy. Cheap for the same reason it is safe: it
    /// returns before the spawn, so no `codex` binary has to exist for this to run.
    #[tokio::test]
    async fn the_codex_runner_refuses_a_restricted_tool_policy() {
        let runner = CodexCliRunner {
            model: "gpt-5.6-terra".to_string(),
        };

        for policy in [ToolPolicy::None, ToolPolicy::McpOnly] {
            let mut request = baseline_run_request();
            request.tool_policy = policy;
            let (session_tx, mut session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let transcript = std::sync::Arc::new(std::sync::Mutex::new(String::new()));

            let refusal = runner
                .run_prompt(request, session_tx, std::sync::Arc::clone(&transcript))
                .await
                .expect_err("a policy the tool cannot apply must not reach a launch");

            assert!(
                refusal.to_string().contains(&format!("{policy:?}")),
                "the refusal must name the policy it could not honour: {refusal}"
            );
            assert!(
                transcript.lock().unwrap().is_empty(),
                "{policy:?}: a refused run must produce no transcript, because nothing ran"
            );
            assert!(
                session_rx.try_recv().is_err(),
                "{policy:?}: a run that never launched must not announce a session"
            );
        }

        // The control, without which a runner that refused EVERY policy would pass the loop above
        // while quietly making the second CLI unusable.
        //
        // `Unrestricted` plus a request the arg builder rejects: the run gets past the policy gate
        // and dies one step later, on `fork_session`. That is the whole point of choosing this
        // request — a control that only set `Unrestricted` would have to spawn `codex` to prove
        // anything, and a unit test must not start an agent on whatever machine it runs on.
        let mut allowed = baseline_run_request();
        allowed.tool_policy = ToolPolicy::Unrestricted;
        allowed.fork_session = true;
        let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let stopped_later = runner
            .run_prompt(
                allowed,
                session_tx,
                std::sync::Arc::new(std::sync::Mutex::new(String::new())),
            )
            .await
            .expect_err("this control request is refused by the arg builder, not by the policy")
            .to_string();

        assert!(
            stopped_later.contains("fork_session"),
            "an unrestricted run must reach the arg builder: {stopped_later}"
        );
        assert!(
            !stopped_later.contains("ToolPolicy"),
            "the tool policy must not be what stops an unrestricted run: {stopped_later}"
        );
    }

    /// The other controls `codex exec` has no counterpart for. Each was accepted and dropped, which
    /// is the one outcome a control must never have: the caller was told the run it asked for
    /// started, and the record agreed.
    ///
    /// `plan_only` is why this is a refusal rather than a warning. It is how a run is made unable to
    /// act — a catch-up run, recovering a schedule the machine slept through, is forced plan-only
    /// precisely because nobody chose for it to run now — so a runner that ignores it converts a
    /// deliberately restrained run into an unrestrained one, in the one case where the operator is
    /// not watching. `mcp_config` is half of a pairing on the Claude path, where the file arrives
    /// with the `--allowedTools` narrowing that keeps the run to that server alone; dropping the flag
    /// drops the narrowing, leaving MORE reachable than was asked for, not less.
    ///
    /// `resume_session_id` is deliberately NOT here. It is unhonourable too, and documented as such
    /// on the runner — but a run resumed on this path merely re-reads its prompt in a fresh session,
    /// which loosens nothing, and refusing it would refuse `session_id` with it: the id the daemon
    /// assigns every run travels the same pair of fields, so the control below would stop being a
    /// control and start being a ban on runs that have a name.
    ///
    /// Asserted against `run_prompt` rather than `codex_cli_args`, because that builder's purity
    /// contract is frozen. Cheap for the same reason it is safe: every case returns before the
    /// spawn, so no `codex` binary has to exist for this to run.
    #[tokio::test]
    async fn the_codex_runner_refuses_flags_it_cannot_honour() {
        let runner = CodexCliRunner {
            model: "gpt-5.6-terra".to_string(),
        };

        let mut restrained = baseline_run_request();
        restrained.plan_only = true;
        let mut narrowed = baseline_run_request();
        narrowed.mcp_config = Some(std::path::PathBuf::from("C:/nucleos/mcp.json"));
        let mut streaming = baseline_run_request();
        streaming.include_partial_messages = true;

        for (field, request) in [
            ("plan_only", restrained),
            ("mcp_config", narrowed),
            ("include_partial_messages", streaming),
        ] {
            let (session_tx, mut session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            let transcript = std::sync::Arc::new(std::sync::Mutex::new(String::new()));

            let refusal = runner
                .run_prompt(request, session_tx, std::sync::Arc::clone(&transcript))
                .await
                .expect_err("a control the tool cannot apply must not reach a launch")
                .to_string();

            assert!(
                refusal.contains(field),
                "the refusal must name the field it could not honour: {refusal}"
            );
            assert!(
                transcript.lock().unwrap().is_empty(),
                "{field}: a refused run must produce no transcript, because nothing ran"
            );
            assert!(
                session_rx.try_recv().is_err(),
                "{field}: a run that never launched must not announce a session"
            );
        }

        // The control. A runner that refused everything would pass the loop above while making the
        // second CLI unusable — and the baseline carries a caller-assigned `session_id`, so this also
        // pins that identity is not what a refusal keys on.
        let honourable = baseline_run_request();
        assert!(
            honourable.session_id.is_some(),
            "the control must carry the identity the daemon assigns every run"
        );
        assert!(
            codex_cli_args(&honourable, "gpt-5.6-terra").is_ok(),
            "a request asking for none of the above must still build a launch"
        );

        // The documented limitation, pinned as a limitation: a resumed run is not refused here, so
        // whoever changes that has to change this line and read why it says so.
        let mut resumed = baseline_run_request();
        resumed.resume_session_id = Some("123e4567-e89b-42d3-a456-426614174001".to_string());
        assert!(
            codex_cli_args(&resumed, "gpt-5.6-terra").is_ok(),
            "resume is degraded on this path, not refused — see the comment in `run_prompt`"
        );
    }

    /// A missing measurement must not read as a measurement of zero — the same invariant
    /// `usage_absent_is_none_not_zero` pins for the Claude CLI and the local runner returns `None`
    /// for by construction.
    ///
    /// These are the `RunOutcome` fields `budget.rs` bills autonomy against. A second runner exists
    /// precisely to be the cheaper path once a spend ceiling has paused the first one, so one that
    /// reported `Some(0)` for a tool that said nothing would make every fallback run look free and
    /// leave the ceiling unable to pause anything.
    ///
    /// Both a plain-text transcript and a structured event carrying no usage object are silence: the
    /// answer is unknown in each, and unknown is not zero.
    #[test]
    fn codex_usage_absent_is_none_not_zero() {
        for stdout in [
            "the crate builds and the suite is green\n",
            r#"{"type":"item.completed","item":{"type":"agent_message","text":"done"}}"#,
        ] {
            let usage = codex_extract_usage(stdout);

            assert_eq!(usage.input_tokens, None, "{stdout}");
            assert_eq!(usage.output_tokens, None, "{stdout}");
            assert_eq!(usage.cache_read_tokens, None, "{stdout}");
        }
    }
}
