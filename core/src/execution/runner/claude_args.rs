//! The Claude CLI's argument list: built-in tools, subagents, `cli_args` and the denied tools.

use super::*;

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
///
/// Re-measured 2026-09-05, reading a live session's advertised tool surface rather than a version
/// number, for the same reason the paragraph above gives: `TaskCreate`, `TaskGet`, `TaskList` and
/// `TaskUpdate` had already moved inside a single version, so pinning this list to a version string
/// again would not have caught the next four either. That pass added `ArtifactCheck`,
/// `ArtifactComments`, `ArtifactData` (siblings of `Artifact`, already here) and `ListAgents`
/// (sibling of `ListMcpResourcesTool`). `scripts/tool-surface.mjs` automates this measurement by
/// hand after a `claude update`; it is not wired into any gate because it needs the CLI installed.
pub const BUILTIN_TOOLS: &[&str] = &[
    "Agent",
    "Artifact",
    "ArtifactCheck",
    "ArtifactComments",
    "ArtifactData",
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
    "ListAgents",
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

/// One helper a conversation may hand work to, as `--agents` takes it.
///
/// The field names ARE the CLI's JSON keys, which is why they are not this codebase's usual prose
/// names: `description` is what the main model reads to decide whether to delegate — the CLI calls
/// it `whenToUse` internally — and `prompt` is the system prompt that helper runs under. Renaming
/// either here would mean a translation layer, and a translation layer is where a key goes missing.
///
/// `name` is carried IN the struct although the flag wants it as the object's key. The window edits
/// a list of helpers and a list has an order and an index; an object has neither, and re-deriving
/// the name from a map key at every layer is how a rename comes to lose a helper.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Subagent {
    /// What the main model calls this helper by.
    ///
    /// `default` because this struct is read back out of the stored object, where the name is the
    /// KEY and not a field — `subagents_from` puts it back. It is serialised normally, because the
    /// window is the other reader of this type and a list of anonymous helpers is not a list
    /// anybody can edit; `agents_json` is the one place that drops it, on its way to the flag.
    #[serde(default)]
    pub name: String,
    /// What this helper is for, in the main model's words. The one field that decides whether it is
    /// ever used at all: the CLI hands this to the parent as the reason to delegate.
    pub description: String,
    /// The system prompt this helper runs under.
    pub prompt: String,
    /// The tools this helper may call, or `None` to inherit the conversation's whole surface.
    ///
    /// Absent is today's behaviour, preserved: a helper defined before this field existed, or one
    /// defined since without naming it, gets everything the parent run has — every call it makes
    /// still comes back through the same `PreToolUse` hook under the parent's `run_id`, which is the
    /// second barrier this field is the first half of. Present grants the CLI exactly this list and
    /// nothing else; `Some(vec![])` is a helper granted no tools at all, a coherent and different
    /// thing from absent, not a shorthand for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tools: Option<Vec<String>>,
    /// Which model answers as this helper, or `None` to inherit the conversation's.
    ///
    /// Absent rather than the CLI's literal `"inherit"`: absence already means it, and offering two
    /// spellings of one state is two states to keep agreeing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// How hard this helper is asked to think, or `None` for whatever its model does by default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// The `--agents` value for a set of helpers: an object keyed by name.
///
/// Public because the door checks the SIZE of what it is about to store, and the size that matters
/// is the size of this string — a helper set is one argv element, and Windows caps a whole command
/// line at 32767 characters. Measuring the struct instead would measure the wrong thing.
///
/// Serialisation cannot fail for these types, so a failure answers with the empty object rather
/// than panicking: no custom helpers is a state the run survives, and it is what a conversation
/// that never defined any already has.
pub fn agents_json(agents: &[Subagent]) -> String {
    let object: serde_json::Map<String, serde_json::Value> = agents
        .iter()
        .filter_map(|agent| {
            let mut body = serde_json::to_value(agent).ok()?;
            // The name is the key here, not a field of the value. Dropped rather than left for the
            // CLI's schema to strip: it strips unknown keys today, and a contract that holds only
            // because the other side is forgiving is one that breaks when it stops being.
            body.as_object_mut()?.remove("name");
            Some((agent.name.clone(), body))
        })
        .collect();
    serde_json::to_string(&object).unwrap_or_else(|_| "{}".to_string())
}

/// The full `claude` argument vector for one run. Pure, so the flags that decide what a run can
/// reach are asserted in tests instead of inspected on a live process.
pub fn cli_args(request: &RunRequest, model: &str) -> Vec<String> {
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
    // Beside `--model`, and conditional like the session flags below it rather than up with
    // `--exclude-dynamic-system-prompt-sections`: that one's comment reserves the position
    // immediately after `--verbose`, and this must not be what pushes it out of place.
    if let Some(effort) = &request.effort {
        args.push("--effort".to_string());
        args.push(effort.clone());
    }
    // Comma-separated, as the CLI takes it. Documented "(only works with --print)", which every run
    // here is: `-p` is the first thing this function pushes and there is no path that omits it.
    if !request.fallback_model.is_empty() {
        args.push("--fallback-model".to_string());
        args.push(request.fallback_model.join(","));
    }
    // `--add-dir` is variadic — it swallows every following argument until the next flag — so it is
    // written here, among the `--flag value` pairs, and never before the prompt. On the argv path
    // the prompt is a POSITIONAL pushed second, and a variadic flag placed above it would eat it.
    if !request.add_dirs.is_empty() {
        args.push("--add-dir".to_string());
        for directory in &request.add_dirs {
            args.push(directory.to_string_lossy().into_owned());
        }
    }
    // Also print-only, and also always satisfied here. A ceiling the CLI enforces itself, which is
    // the difference from `max_turns`: that one has no flag and is counted from the transcript.
    if let Some(ceiling) = request.max_budget_usd {
        args.push("--max-budget-usd".to_string());
        args.push(format!("{ceiling}"));
    }
    // One argv element holding a JSON object, which is how the flag is defined — not a repeatable
    // `--agents name=…`. Empty writes nothing rather than `{}`: the CLI answers unparseable JSON
    // with an empty agent list and no error, so the difference between "no flag" and "a flag that
    // parsed to nothing" is invisible from outside, and only one of them is a state anybody chose.
    if !request.agents.is_empty() {
        args.push("--agents".to_string());
        args.push(agents_json(&request.agents));
    }
    // Appended, never substituted — see the field. Written among the flag/value pairs like the
    // rest, and never above the prompt, for the reason `--add-dir` gives.
    if let Some(instructions) = &request.append_system_prompt {
        args.push("--append-system-prompt".to_string());
        args.push(instructions.clone());
    }
    // Beside the session flags below in meaning, and written here in position for the same reason
    // everything conditional is: `--exclude-dynamic-system-prompt-sections` holds the slot right
    // after `--verbose` so the prompt cache keeps matching, and nothing may push it out of place.
    if let Some(name) = &request.session_name {
        args.push("--name".to_string());
        args.push(name.clone());
    }
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
    // One flag, one value, written for every run. Two booleans used to compete for this line and
    // an `else if` decided which of them won; the ordering is gone because the choice is made once,
    // where the run is started, and arrives here already made.
    //
    // Unconditional, where the old form wrote nothing for an ordinary run — and "no flag" stopped
    // being a known state. From v2.1.228 the default start-up mode on Pro/Max/Team plans is `auto`,
    // the CLI's own classifier model: billed, slower, and refusing things this application cannot
    // explain to anybody. A `PreToolUse` allow does not skip it. Saying `manual` out loud costs one
    // changed prompt-cache prefix, once, and buys a behaviour that does not depend on the account's
    // plan or on a default that can move without notice.
    //
    // Still after `--exclude-dynamic-system-prompt-sections` and never before it: that one is
    // pinned immediately behind `--verbose` so the cache prefix stays stable across back-to-back
    // job nodes.
    args.push("--permission-mode".to_string());
    args.push(request.permission.cli_value().to_string());
    if let Some(path) = &request.mcp_config {
        args.push("--mcp-config".to_string());
        args.push(path.to_string_lossy().into_owned());
        args.push("--allowedTools".to_string());
        args.push(match &request.allowed_mcp_tools {
            None => "mcp__nucleos__*".to_string(),
            Some(names) => names
                .iter()
                .map(|name| format!("mcp__nucleos__{name}"))
                .collect::<Vec<_>>()
                .join(","),
        });
    }
    // Every policy runs strict: the MCP servers this user happens to have configured are nobody's
    // here — the ambient surface includes file-writing connectors, and each server is re-described
    // in full on every turn. A chat's extra servers come in through `--mcp-config` above, beside
    // nucleos (spec §8.2), so the strict flag never costs a run a server it was meant to have.
    args.push("--strict-mcp-config".to_string());
    // ONE `--disallowedTools`, holding everything anything wanted denied.
    //
    // The policy's denials and the run's own used to be unable to coexist, and the failure would
    // have been silent: `--disallowedTools` is variadic, so a second occurrence REPLACES the first
    // rather than adding to it. Written twice, a conversation asking not to run `Bash` would have
    // taken `ToolPolicy::McpOnly` down with it and come back with the whole built-in tool set.
    let denied = denied_tools(&request.tool_policy, &request.denied_tools);
    if !denied.is_empty() {
        args.push("--disallowedTools".to_string());
        args.push(denied.join(","));
    }
    args
}

/// Everything one run must be denied: what its policy denies, plus what it asked to be denied.
///
/// Separate from `cli_args` so the merge itself is assertable — this is the function whose being
/// wrong would look like a safety property holding, and a test that read an argument vector could
/// only ever check the flag that survived.
///
/// `ToolPolicy::None` answers with the wildcard alone. Measured against CLI 2.1.198 it yields an
/// `init` event advertising NO tools at all — the capability is absent rather than refused, so
/// there is nothing for a prompt injected into a mail body to talk the model into reaching for —
/// and naming individual tools beside `*` would only add stderr lines about rules that match
/// nothing on top of a denial that already covers them.
pub(super) fn denied_tools(policy: &ToolPolicy, asked: &[String]) -> Vec<String> {
    if *policy == ToolPolicy::None {
        return vec!["*".to_string()];
    }
    let mut denied: Vec<String> = match policy {
        ToolPolicy::McpOnly => BUILTIN_TOOLS.iter().map(|name| name.to_string()).collect(),
        _ => Vec::new(),
    };
    for name in asked {
        if !denied.iter().any(|have| have == name) {
            denied.push(name.clone());
        }
    }
    denied
}
