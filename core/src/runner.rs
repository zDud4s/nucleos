use async_trait::async_trait;
use base64::Engine;
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

/// PURE fold: how many model responses this stream has carried, one line at a time.
///
/// One counter for both CLIs. `codex exec` says `turn.completed` once per turn. Claude says
/// `assistant` once per content BLOCK, not once per message: an answer holding some text and two
/// tool calls arrives as three `assistant` events carrying the same `message.id` and the same
/// `usage`. Measured on this daemon's own runs against CLI 2.1.263: 32 events for 14 messages on
/// run 900473, 200 for 125 on run 900463. Counting events is what stopped 900463 at 125 responses
/// under a ceiling that says 200, and it fell hardest on the runs that call the most tools at once,
/// which is nothing a brake on motion should care about.
///
/// So a Claude event counts once per id. A set rather than only the last id seen: nothing here then
/// depends on the blocks of one message arriving next to each other, and the price is one short
/// string per response. An `assistant` event with no id counts on its own, as every event did
/// before ids were read.
///
/// Neither event name appears in the other CLI's stream, so one fold cannot double-count — and the
/// alternative, a counter per CLI, is how a ceiling ends up enforced on one path and quietly absent
/// on the other, which is worse than no ceiling because somebody will believe it is there.
///
/// Counted from the transcript rather than asked of the CLI: measured against CLI 2.1.198, there is
/// no `--max-turns` flag to delegate this to. `--max-budget-usd` exists and is a different brake —
/// money, which the job already has, rather than motion, which nothing had.
#[derive(Debug, Default)]
pub(crate) struct TurnCounter {
    count: i64,
    seen: std::collections::HashSet<String>,
}

impl TurnCounter {
    /// Folds one line in, and answers whether it began a response this counter had not seen yet.
    pub(crate) fn line(&mut self, line: &str) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            return false;
        };
        let began = match value.get("type").and_then(serde_json::Value::as_str) {
            Some("turn.completed") => true,
            Some("assistant") => match value
                .get("message")
                .and_then(|message| message.get("id"))
                .and_then(serde_json::Value::as_str)
            {
                Some(id) => self.seen.insert(id.to_string()),
                None => true,
            },
            _ => false,
        };
        if began {
            self.count = self.count.saturating_add(1);
        }
        began
    }

    /// The responses counted so far.
    pub(crate) fn count(&self) -> i64 {
        self.count
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
    /// Whether the CLI summarised its own context at some point during this run.
    ///
    /// Beside the numbers rather than derived from them, because it cannot be derived from them: a
    /// compacted turn's `context_fill` is simply lower than the one before it, which is
    /// indistinguishable from a short question. The stream says it outright and this carries what
    /// it said.
    pub compacted: bool,
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

/// Which `--permission-mode` the CLI is launched with, and NOTHING else.
///
/// One value where two booleans used to race. `plan_only` and `classifier_governs_tools` could both
/// be true, so `cli_args` had an `else if` deciding which of them got to write the flag — and the
/// test that pinned that order (`plan_only_outranks_the_classifier_permission_surface`) said in its
/// own words that "order is not where a safety property belongs". It is not there any more: a run
/// that must not act cannot be handed `bypassPermissions` because there is one field and it holds
/// one value, chosen once, at the call that starts the run.
///
/// Deliberately NOT `chats::PermissionMode`, which has six values and is the CONVERSATION's
/// policy. `Manual`, `Auto` and `DontAsk` all three project onto `Default` here, because what
/// separates them lives in the `PreToolUse` hook and not on a command line. Keeping the two types
/// apart is what stops somebody answering one question with the other.
///
/// Called `Default` and not `Auto` so nobody has to wonder why an autopilot run carries a
/// conversation's policy: it is the rung with no elevation, which is what every run that is not a
/// chat turn wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permission {
    /// No elevation: the CLI decides nothing on its own and the hook decides everything.
    Default,
    /// Edits in scope go through without asking; everything else still asks.
    AcceptEdits,
    /// The run answers with a plan. It is how a run is made unable to act — a catch-up run,
    /// recovering a schedule the machine slept through, is forced into it precisely because nobody
    /// chose for it to run NOW.
    Plan,
    /// The CLI's permission barrier stands down, on the strength of the classifier taking over.
    ///
    /// Only ever built where that classifier is verified present — see
    /// `autopilot::classifier_hook_is_wired`. Standing this barrier down without the one that
    /// replaces it leaves a run governed by nothing at all.
    Bypass,
}

impl Permission {
    /// The spelling the CLI accepts, MEASURED rather than chosen.
    ///
    /// ```text
    /// $ claude --version   -> 2.1.260 (Claude Code)
    /// $ claude --help      -> --permission-mode <mode>  (choices: "acceptEdits", "auto",
    ///                           "bypassPermissions", "manual", "dontAsk", "plan")
    /// ```
    ///
    /// **`default` is not on that list**, which is why `Default` writes `manual`: the documentation
    /// calls this rung `default` and the command line does not accept the word. Writing it would
    /// have failed the start of every run this daemon launches.
    ///
    /// **And no mode of this house ever writes `auto`.** That one is the CLI's OWN classifier
    /// model, a second opinion nobody here reconciled with the classifier this daemon runs; asking
    /// for both would be paying twice for two judgements. `manual` is exactly "decide nothing, the
    /// hook decides" — which is the posture we want FROM THE CLI whatever rung the conversation is
    /// on.
    ///
    /// **`dontAsk` on that list is not our `dont_ask` either**, and the collision of names is the
    /// reason this paragraph exists. `chats::PermissionMode::DontAsk` is a rung of THIS house,
    /// enforced entirely by the hook, and it launches `manual` like the two rungs beside it. The
    /// CLI's `dontAsk` is the CLI's own idea of not asking, decided by a surface we do not control
    /// and cannot see the reasons of. Selecting it would move the decision off the classifier and
    /// onto that surface, which is the one thing every value in this enum is arranged to avoid.
    pub fn cli_value(self) -> &'static str {
        match self {
            Self::Default => "manual",
            Self::AcceptEdits => "acceptEdits",
            Self::Plan => "plan",
            Self::Bypass => "bypassPermissions",
        }
    }

    /// Which rung a conversation's policy launches the CLI on.
    ///
    /// Lossy on purpose, and the loss is the point: `Manual`, `Auto` and `DontAsk` are the same
    /// command line and differ only in what the hook lets through — `Manual` asks about every
    /// mutation, `Auto` asks only about what the classifier does not recognise, and `DontAsk`
    /// refuses that same remainder instead of asking about it. Three policies, one command line.
    pub fn for_chat(mode: crate::chats::PermissionMode) -> Self {
        match mode {
            crate::chats::PermissionMode::Manual
            | crate::chats::PermissionMode::Auto
            | crate::chats::PermissionMode::DontAsk => Self::Default,
            crate::chats::PermissionMode::AcceptEdits => Self::AcceptEdits,
            crate::chats::PermissionMode::Plan => Self::Plan,
            crate::chats::PermissionMode::Bypass => Self::Bypass,
        }
    }
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
    /// Which `--permission-mode` this run is launched with.
    ///
    /// Chosen once, by the caller that knows what kind of run this is. It replaced a `plan_only`
    /// boolean that shared the flag with `classifier_governs_tools` and needed an ordering rule
    /// between them to stay safe.
    pub permission: Permission,
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
    /// Pictures travelling with this run's opening turn. Empty for every run that carries none.
    ///
    /// Only ever read on the stdin path: an argument vector holds a string and there is nowhere in
    /// it for bytes to go. A run given images and not `steerable` would silently drop them, so the
    /// two are decided together at the one place that builds a turn with a picture in it.
    pub images: Vec<Attachment>,
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
    pub messages: Option<tokio::sync::mpsc::UnboundedReceiver<LaterTurn>>,
    /// Whether this run opts in to the operator's ambient MCP surface.
    ///
    /// `false` everywhere today, and that is the point: the strict default closes the exfiltration
    /// path a hostile mail body could otherwise use to reach a file-writing connector the daemon
    /// never asked for. A run pays for every ambient server in re-sent tool definitions on every
    /// turn, so the cost falls on whoever asks rather than on everyone who did not.
    pub ambient_mcp: bool,
    /// Per-run override of the runner's configured model. `None` keeps it.
    pub model: Option<String>,
    /// How hard this run asks the model to think, or `None` for the CLI's own default.
    ///
    /// `low | medium | high | xhigh | max`, validated where it is CHOSEN and not here: this struct
    /// is built by seven callers and a check in each is seven places for the list to drift. What
    /// reaches this field has already been checked against `config::EFFORT_LEVELS`, and a value
    /// that somehow was not is refused by the CLI at spawn — loudly, which is the safe direction.
    ///
    /// Honoured only by the agent CLI. `OllamaRunner` has no such notion and drops it, which is why
    /// a local choice carries no effort levels rather than letting the window offer a dial that
    /// turns nothing.
    pub effort: Option<String>,
    /// Who answers when the chosen model is overloaded or unavailable, tried in the order given.
    ///
    /// Beside `model` and not inside it: one says who SHOULD answer and the other who may answer
    /// instead, and one string holding both would make "no fallback" and "no model" the same
    /// absence. Empty is no fallback, which is what every run in this daemon has always had.
    pub fallback_model: Vec<String>,
    /// Directories this run's tools may reach beyond its `cwd`. Empty is the established behaviour.
    ///
    /// `cwd` is where the run happens; these are places it may also look. The distinction matters
    /// because only one of them can be the working directory, and a person working across a
    /// repository and the notes folder beside it should not have to choose which half is visible.
    pub add_dirs: Vec<PathBuf>,
    /// The most one invocation of the CLI may spend, or `None` for no ceiling.
    ///
    /// Per RUN, which in this daemon is per turn — the CLI's own flag bounds a single invocation.
    /// It is emphatically not a conversation total: ten turns at the ceiling cost ten times it. The
    /// column behind it is named `turn_budget_usd` for the same reason.
    ///
    /// A ceiling the CLI enforces, unlike `max_turns` a few fields up, which this daemon counts
    /// itself because the CLI has no flag for it.
    pub max_budget_usd: Option<f64>,
    /// The helpers this run may hand work to, beyond the ones the CLI finds in `.claude/agents/`.
    ///
    /// Additive, not a replacement: measured against CLI 2.1.198, `--agents` builds definitions
    /// tagged `flagSettings` and merges them with the ones discovered on disk. A conversation with
    /// its own reviewer still has the project's.
    ///
    /// Empty is the established behaviour and writes no flag at all — which matters more here than
    /// elsewhere, because the CLI parses this JSON in a try/catch and answers a throw with an EMPTY
    /// agent list. Writing `--agents {}` would therefore not be a harmless no-op to reason about.
    ///
    /// Honoured only by the agent CLI, like `effort`. `OllamaRunner` has no notion of a subagent.
    pub agents: Vec<Subagent>,
    /// Standing instructions for this run, appended to the CLI's own system prompt.
    ///
    /// APPENDED and never substituted. `--system-prompt` exists too and is deliberately not
    /// reachable from here: it replaces the CLI's own, which carries the tool descriptions and the
    /// safety framing, and a run that lost those reads as a run whose model got worse.
    ///
    /// Per invocation, which in this daemon is per turn — so a conversation's instructions travel
    /// on every one of its turns. Sending them only on the first would make them apply to the
    /// opening message and quietly stop mattering, which is the hardest kind of wrong to notice
    /// because the first answer is right.
    pub append_system_prompt: Option<String>,
    /// Built-in tools this run may not reach for, on top of whatever `tool_policy` already denies.
    ///
    /// Only ever takes something away. The allow-listing flag beside it does not restrict anything
    /// — it GRANTS permission on top of what is already allowed, which `BUILTIN_TOOLS` measured —
    /// so there is no widening version of this field to get wrong.
    ///
    /// Merged with the policy's own denials into ONE flag by `cli_args`. `--disallowedTools` is
    /// variadic, so a second occurrence REPLACES the first: two flags would be a per-run preference
    /// silently undoing a safety property.
    pub denied_tools: Vec<String>,
    /// What to call this run's session where the CLI shows sessions, or `None` for nameless.
    ///
    /// Cosmetic and nothing else: it reaches the `--resume` picker and the terminal title, and no
    /// decision anywhere depends on it. Carried because every session this daemon has ever minted
    /// is nameless there, so a person looking at their own machine sees a wall of timestamps where
    /// this app's conversations are.
    pub session_name: Option<String>,
    /// The context window this run is given, or `None` for whatever the CLI decides on its own.
    ///
    /// An ENV VAR and not a flag, because the CLI has no flag for it —
    /// `CLAUDE_CODE_AUTO_COMPACT_WINDOW`, which it clamps to 100k–1M and then caps at the model's
    /// real window. Below that window minus 13k the CLI compacts its own context and carries on in
    /// the same session, which is how the editor has always behaved and what this daemon used to
    /// approximate by refusing to resume and starting again.
    ///
    /// Verified in headless mode rather than assumed to be a REPL feature: `claude -p --resume`
    /// with the window forced low emits `{"type":"system","subtype":"status","status":"compacting"}`
    /// followed by a `compact_result`. Both are read back below, so a compaction is something the
    /// transcript can show rather than something that silently happened.
    ///
    /// `None` on every run that is not a conversation. A one-shot run has no second turn for a
    /// compaction to serve, and naming a window for it would only move the point at which a single
    /// long tool loop starts summarising itself.
    pub context_window: Option<i64>,
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

/// A picture travelling with a turn, as the API carries one.
///
/// Base64 rather than bytes, because base64 is what goes on the wire in both directions: it arrives
/// that way from the window and leaves that way to the CLI, and decoding in between would be work
/// done only to be undone.
///
/// `media_type` is the sender's claim about what these bytes are, and it is passed on as a claim.
/// Nothing here sniffs the content: a run reading a picture is reading it either way, and a daemon
/// that second-guessed the label would be deciding on behalf of a model that can see the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attachment {
    pub media_type: String,
    pub data: String,
}

/// A turn arriving after the one a run was launched with.
///
/// Text AND pictures, because for a conversation that keeps its process every turn after the first
/// is one of these — and a screenshot pasted into the second is the same kind of thing as one
/// attached to the first. It was a bare `String` while this channel existed only for the queue
/// drain, where a later turn really was text somebody typed while waiting; a conversation that keeps
/// its CLI had to give the CLI up the moment anybody pasted an image, which is exactly when a coding
/// conversation is most alive.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LaterTurn {
    pub text: String,
    /// Empty for almost every turn, and the reason this is a struct rather than a `String`.
    pub images: Vec<Attachment>,
}

/// One line of `--input-format stream-json` stdin: a single user turn.
///
/// Measured against CLI 2.1.198, this shape is accepted and the run proceeds — the `init` event fires
/// and the process exits 0. Built through `serde_json` rather than `format!` because a turn is
/// delimited by a newline: a prompt containing one, or a quote, would otherwise arrive as two
/// half-parsed lines instead of the single instruction it is.
pub(crate) fn user_message_line(text: &str, images: &[Attachment]) -> String {
    // A plain string when there is nothing to carry, and that is not tidiness: the string form is
    // the one measured working, and every run in this daemon that is not a chat uses it. Rewriting
    // them all as arrays to make one new case uniform would change what is proven to make room for
    // what is not.
    //
    // An array when there is. The image comes first and the words after — the order the API
    // documents for a question about a picture, and the order a person types in.
    let content = match images.is_empty() {
        true => serde_json::Value::String(text.to_string()),
        false => {
            let mut blocks: Vec<serde_json::Value> = images
                .iter()
                .map(|image| {
                    serde_json::json!({
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": image.media_type,
                            "data": image.data,
                        },
                    })
                })
                .collect();
            blocks.push(serde_json::json!({ "type": "text", "text": text }));
            serde_json::Value::Array(blocks)
        }
    };
    let mut line = serde_json::json!({
        "type": "user",
        "message": { "role": "user", "content": content },
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
///
/// Re-measured 2026-09-05, reading a live session's advertised tool surface rather than a version
/// number, for the same reason the paragraph above gives: `TaskCreate`, `TaskGet`, `TaskList` and
/// `TaskUpdate` had already moved inside a single version, so pinning this list to a version string
/// again would not have caught the next four either. That pass added `ArtifactCheck`,
/// `ArtifactComments`, `ArtifactData` (siblings of `Artifact`, already here) and `ListAgents`
/// (sibling of `ListMcpResourcesTool`). `scripts/tool-surface.mjs` automates this measurement by
/// hand after a `claude update`; it is not wired into any gate because it needs the CLI installed.
pub(crate) const BUILTIN_TOOLS: &[&str] = &[
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
        // No tool denial of its own — the classifier governs what an autopilot run may call — but
        // the ambient MCP servers are nobody's: each one is re-described in full on every turn, and
        // nothing in the daemon's design calls them. The `--mcp-config` block above still runs, so
        // a run carrying `request.mcp_config` keeps its nucleos server under the strict flag.
        //
        // Only this arm is opt-out-able. `McpOnly` and `None` keep their unconditional strict flag,
        // where it is a safety property rather than an economy.
        ToolPolicy::Unrestricted => {
            if !request.ambient_mcp {
                args.push("--strict-mcp-config".to_string());
            }
        }
        // Both of the arms below drop every MCP server this user happens to have configured — the
        // ambient surface a spawned run inherits otherwise includes file-writing connectors. Under
        // the wildcard it is redundant and passed anyway, so a future narrowing of one is not a
        // silent widening of the other.
        ToolPolicy::McpOnly | ToolPolicy::None => {
            args.push("--strict-mcp-config".to_string());
        }
    }
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
fn denied_tools(policy: &ToolPolicy, asked: &[String]) -> Vec<String> {
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
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
            let text = match v.get("type").and_then(|t| t.as_str()) {
                Some("result") => v.get("result").and_then(|r| r.as_str()),
                Some("item.completed")
                    if v.get("item")
                        .and_then(|item| item.get("type"))
                        .and_then(|kind| kind.as_str())
                        == Some("agent_message") =>
                {
                    v.get("item")
                        .and_then(|item| item.get("text"))
                        .and_then(|text| text.as_str())
                }
                _ => None,
            };
            if let Some(text) = text.filter(|text| !text.trim().is_empty()) {
                reply = Some(text.to_string());
            }
        }
    }
    reply
}

/// Whether a `claude -p --output-format stream-json` run ended on an API error that says nothing
/// about the work: the network, or the service being busy or down.
///
/// Read from the LAST `result` event and from nowhere else, beside `extract_reply` for the reason
/// that function gives: knowing the CLI's output format is this module's job. True only when that
/// event says `is_error: true` with `terminal_reason: "api_error"`, and the status the CLI got back
/// is one a second attempt can get past — none at all (nothing answered: DNS, a dropped
/// connection), 408, 429, or any 5xx, 529 "overloaded" among them. Any other 4xx is the request
/// itself being refused, which it will be again, and a turn that ended for any other reason
/// (`max_turns`, a hook) ended on something the work did.
///
/// Measured on job 26's review, run 900483, 2026-09-13: ten `api_retry` events, then a result line
/// with `terminal_reason: "api_error"`, `api_error_status: null` and "API Error: Can't reach the
/// API server — check your internet or DNS (ENOTFOUND)". Its `subtype` said `success`: only
/// `is_error` and `terminal_reason` told the truth, which is why neither `subtype` nor the exit
/// code is read.
pub(crate) fn failed_on_a_transient_api_error(stdout: &str) -> bool {
    let Some(result) = stdout.lines().rev().find_map(|line| {
        serde_json::from_str::<serde_json::Value>(line.trim())
            .ok()
            .filter(|event| event.get("type").and_then(|kind| kind.as_str()) == Some("result"))
    }) else {
        return false;
    };
    if result.get("is_error").and_then(|flag| flag.as_bool()) != Some(true)
        || result
            .get("terminal_reason")
            .and_then(|reason| reason.as_str())
            != Some("api_error")
    {
        return false;
    }
    match result.get("api_error_status") {
        None | Some(serde_json::Value::Null) => true,
        Some(status) => status
            .as_u64()
            .is_some_and(|code| code == 408 || code == 429 || (500..=599).contains(&code)),
    }
}

/// Job 26's review, run 900483, cut down to what the detector above reads: the first of its ten
/// retries, and its result line with the zeroed counters left out. Shared with `job.rs`, whose
/// tests seed a review that printed exactly this.
#[cfg(test)]
pub(crate) const REVIEW_THAT_NEVER_REACHED_THE_API: &str = r#"{"type":"system","subtype":"api_retry","attempt":1,"max_retries":10,"retry_delay_ms":614,"error_status":null,"error":"unknown","session_id":"f4a94b9c-f0fe-484b-9514-9fefa640a6b6"}
{"stop_reason":"stop_sequence","session_id":"f4a94b9c-f0fe-484b-9514-9fefa640a6b6","total_cost_usd":0,"terminal_reason":"api_error","is_error":true,"num_turns":1,"subtype":"success","api_error_status":null,"result":"API Error: Can't reach the API server — check your internet or DNS (ENOTFOUND)","type":"result","duration_ms":172362}"#;

/// A turn as it stands PART WAY THROUGH: what has been written, and what is being done.
#[derive(Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct LiveTurn {
    /// The answer so far. Empty means nothing has been said yet, which on a live turn is not the
    /// same claim as a turn that answered with nothing.
    pub text: String,
    /// The tool being run right now, or `None` when the model is writing rather than acting.
    pub doing: Option<String>,
    /// Roughly how many tokens the model spent thinking, or `None` when it did not think or the
    /// stream never said.
    ///
    /// The only thing about a thought that this machine can actually have. The CLI emits the
    /// signature and a running `thinking_tokens` estimate and WITHHOLDS the text: asked of a real
    /// interactive session, 610 thinking blocks, every one of them `thinking: ""`. So a window that
    /// offered to show the reasoning would be offering something nothing here holds — and this is
    /// what it says instead, which is true.
    ///
    /// An estimate, and named one. It is the CLI's own running count, and what it has to be right
    /// about is whether the model deliberated and roughly how hard.
    pub thought_tokens: Option<i64>,
    /// What the model thought before it answered, oldest first.
    ///
    /// Empty in practice on every stream this daemon has seen, for the reason above — the parse is
    /// here so that the day the CLI stops withholding the words, they arrive. Empty is therefore
    /// the ordinary case and not a failure, and nothing downstream may read it as one.
    ///
    /// Apart from `text` and never joined to it. Thinking is not the reply: it is working, often
    /// wrong on the way to being right, and a window that concatenated the two would record the
    /// model's private reasoning as the thing it said — which is then what a replay quotes back to
    /// it, and what a person reads as its answer.
    pub thought: Vec<String>,
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
    /// The plan this call wrote, when the call was one that writes plans. Empty for every other.
    ///
    /// The exception to "the one argument": a `TodoWrite` carries no path and no command, so
    /// `detail_of` finds nothing and the call used to arrive as a bare name with nothing beside it
    /// — while what it actually carried was the whole plan. A model working through a list is the
    /// shape of most real work, and none of it reached the page.
    ///
    /// Defaulted on the way in. `tools_used` is stored JSON and every turn already recorded is a
    /// row without this field; a row that predates the plan reads as a call that wrote none, which
    /// is exactly what it was.
    #[serde(default)]
    pub todos: Vec<Todo>,
    /// What the tool answered, cut to `RESULT_LIMIT` characters, or `None` when nothing came back.
    ///
    /// Until this existed a turn said what it REACHED FOR and never what it found: `Bash` beside
    /// `cargo test dates::` with no way to learn, from the conversation, whether the tests passed.
    /// The model's paragraph underneath is a summary of this, and a summary is exactly the thing
    /// somebody opening a tool call has decided not to take on trust.
    ///
    /// Cut, because a `Read` of a three-thousand-line file answers with the file. The full length
    /// is kept beside it in `result_chars`, so the window can say what it is NOT showing rather
    /// than present a truncation as the whole answer.
    ///
    /// **Not on the transcript.** `ToolCall::without_result` strips this before the turn list is
    /// serialised, and the answers are fetched per turn on request — the transcript route is
    /// polled at a live turn's cadence, and a hundred turns of tool output on a one-second poll
    /// is a cost paid forever for something almost nobody has open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// How long the whole answer was, in characters. `None` when nothing came back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_chars: Option<i64>,
    /// Whether the tool answered with an error rather than an answer.
    ///
    /// Its own field and not inferred from the text: "the command failed" and "the command printed
    /// something that mentions an error" are different facts, and only the stream knows which this
    /// was. False on every turn recorded before the field existed, which is the honest default —
    /// nothing about those rows says a tool failed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub result_failed: bool,
}

impl ToolCall {
    /// The same call with its answer removed, for the transcript. See `result`.
    pub(crate) fn without_result(self) -> Self {
        Self {
            result: None,
            result_chars: None,
            result_failed: false,
            ..self
        }
    }
}

/// One line of a plan.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Todo {
    pub text: String,
    /// As the CLI words it: `pending`, `in_progress`, `completed`. Kept as it arrives rather than
    /// mapped to something of this daemon's own — a fourth state invented upstream would otherwise
    /// silently become one of the three here.
    pub status: String,
}

/// The plan inside a `TodoWrite` input, or nothing at all for every other tool.
///
/// Anything shaped wrong is skipped rather than guessed at: a plan drawn from a half-understood
/// input is a list of work nobody planned.
fn plan_of(name: &str, input: Option<&serde_json::Value>) -> Vec<Todo> {
    if name != "TodoWrite" {
        return Vec::new();
    }
    let Some(items) = input
        .and_then(|input| input.get("todos"))
        .and_then(|v| v.as_array())
    else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let text = item.get("content").and_then(|v| v.as_str())?.trim();
            if text.is_empty() {
                return None;
            }
            Some(Todo {
                text: cut_detail(text),
                status: item
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("pending")
                    .to_string(),
            })
        })
        .collect()
}

/// The longest detail kept. A command line can be a heredoc.
const DETAIL_LIMIT: usize = 120;

/// The longest tool answer kept.
///
/// Two thousand characters is about thirty lines: enough for a test summary, a short diff or the
/// head of a compiler's complaint, which is what somebody opening a tool call is looking for. A
/// `Read` answers with a whole file and a `Grep` with every hit, and neither belongs in a row of
/// a database that is read back in full every time a conversation is opened.
const RESULT_LIMIT: usize = 2000;

/// What a `tool_result` block actually said, flattened.
///
/// The CLI sends `content` two ways — a bare string, or an array of content blocks — and both are
/// ordinary. Anything else comes back as `None` rather than as a JSON dump: a window showing the
/// serialisation of a shape this daemon did not recognise is worse than one showing nothing.
fn result_text(content: Option<&serde_json::Value>) -> Option<String> {
    match content? {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(blocks) => {
            let joined = blocks
                .iter()
                .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                .collect::<Vec<_>>()
                .join("\n");
            (!joined.is_empty()).then_some(joined)
        }
        _ => None,
    }
}

/// The argument of a tool call worth showing beside its name.
///
/// A fixed list of keys tried in order, rather than "the first string in the object": the input
/// keys belong to the tools, and an unknown tool would otherwise contribute whichever field
/// happened to be ordered first — a different answer between two runs of the same call.
pub(crate) fn detail_of(input: &serde_json::Value) -> Option<String> {
    // `description` last, and last on purpose: it is what a `Task` carries and nothing else does,
    // and a tool that also says where it acted must answer with that instead. A key ordered above
    // it would make the sentence a model wrote win over the file it opened.
    //
    // `notebook_path` is here because `NotebookEdit` names its target with it and nothing else in
    // this list matched, so every notebook write showed as a bare tool name with no file beside
    // it. That is cosmetic on an allow and it is not cosmetic on an approval: a person was being
    // asked to permit a write without being told what it writes, which is not a question anyone
    // can answer. It sits beside `file_path` because it IS the file path, under the one tool that
    // spells it differently.
    const KEYS: [&str; 8] = [
        "file_path",
        "notebook_path",
        "path",
        "command",
        "pattern",
        "url",
        "query",
        "description",
    ];
    let found = KEYS
        .iter()
        .find_map(|key| input.get(key).and_then(|value| value.as_str()))?;
    let trimmed = found.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(cut_detail(trimmed))
}

/// The same ceiling for a detail and for a line of a plan: both are one line beside a tool's name,
/// and a plan item can be a paragraph somebody pasted.
fn cut_detail(text: &str) -> String {
    let mut out: String = text.chars().take(DETAIL_LIMIT).collect();
    if text.chars().count() > DETAIL_LIMIT {
        out.push('…');
    }
    out
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
    let mut thought: Vec<String> = Vec::new();
    let mut thought_tokens: Option<i64> = None;
    let mut pondering = String::new();
    let mut doing: Option<String> = None;
    let mut did: Vec<ToolCall> = Vec::new();
    // The `tool_use` id of each call in `did`, by the same index. Parallel rather than a field on
    // `ToolCall`, because the id is a fact about this stream and not about the call: it is used to
    // pair an answer with the question that asked it, and then it is finished with. A field would
    // put it in the database and in the window, where nothing would ever read it.
    let mut called: Vec<Option<String>> = Vec::new();

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
                // Thinking streams on the same channel under its own key — and arrives EMPTY.
                // The CLI sends `thinking_delta`s carrying `"thinking": ""` and a token estimate,
                // never the words. Read anyway, so the day it stops withholding them they appear;
                // measured below, because the measurement is the part that exists.
                if let Some(text) = value
                    .pointer("/event/delta/thinking")
                    .and_then(|t| t.as_str())
                {
                    pondering.push_str(text);
                }
                thought_tokens = larger(
                    thought_tokens,
                    value.pointer("/event/delta/estimated_tokens"),
                );
            }
            // The one line that says anything real about a thought. A running total, and it arrives
            // even on the turns whose thinking block came through with its text stripped out —
            // which, so far, is all of them.
            Some("system")
                if value.get("subtype").and_then(|s| s.as_str()) == Some("thinking_tokens") =>
            {
                thought_tokens = larger(thought_tokens, value.get("estimated_tokens"));
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
                thought.extend(
                    blocks
                        .iter()
                        .filter(|block| {
                            block.get("type").and_then(|t| t.as_str()) == Some("thinking")
                        })
                        .filter_map(|block| block.get("thinking").and_then(|t| t.as_str()))
                        .filter(|text| !text.trim().is_empty())
                        .map(str::to_string),
                );
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
                        todos: plan_of(name, block.get("input")),
                        result: None,
                        result_chars: None,
                        result_failed: false,
                    });
                    called.push(
                        block
                            .get("id")
                            .and_then(|id| id.as_str())
                            .map(str::to_string),
                    );
                    doing = Some(name.to_string());
                }
                // The message that just completed is the one those deltas were writing — both
                // kinds of them. The CLI sends one `assistant` event per API message carrying every
                // block of it, so a message that ended a thought carries that thought, and keeping
                // the buffer as well would show it twice.
                writing.clear();
                pondering.clear();
            }
            // A tool answering is the only thing that ends a tool call. Clearing this anywhere else
            // would show the model as writing while a command is still running.
            Some("user") => {
                let mut returned = false;
                for block in value
                    .pointer("/message/content")
                    .and_then(|c| c.as_array())
                    .into_iter()
                    .flatten()
                    .filter(|block| {
                        block.get("type").and_then(|t| t.as_str()) == Some("tool_result")
                    })
                {
                    returned = true;
                    // Paired by id and never by position. A turn can have two tool calls in flight
                    // at once — the CLI runs them concurrently — and answers arrive in whatever
                    // order the tools finish, so "the most recent call" is wrong exactly when it
                    // matters. An answer whose id names no call this stream made is dropped: it
                    // belongs to something that is not in this list.
                    let Some(index) = block
                        .get("tool_use_id")
                        .and_then(|id| id.as_str())
                        .and_then(|id| called.iter().position(|made| made.as_deref() == Some(id)))
                    else {
                        continue;
                    };
                    let Some(text) = result_text(block.get("content")) else {
                        continue;
                    };
                    let call = &mut did[index];
                    call.result_chars = Some(text.chars().count() as i64);
                    call.result = Some(text.chars().take(RESULT_LIMIT).collect());
                    call.result_failed = block
                        .get("is_error")
                        .and_then(|flag| flag.as_bool())
                        .unwrap_or(false);
                }
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
    // A thought still being written when the stream was read is worth showing — that is most of
    // what a live turn IS while it is hard.
    if !pondering.trim().is_empty() {
        thought.push(pondering);
    }

    LiveTurn {
        text: finished.join(
            "

",
        ),
        thought_tokens,
        thought,
        doing,
        did,
    }
}

/// The larger of what is known and what a line claims, ignoring a line that claims nothing.
///
/// `estimated_tokens` is a RUNNING total and arrives on two different kinds of line, one of which
/// sometimes sends it null. Taking the largest is what makes a stream read at any point report the
/// whole thought so far rather than whichever line happened to come last.
fn larger(known: Option<i64>, claimed: Option<&serde_json::Value>) -> Option<i64> {
    match claimed.and_then(serde_json::Value::as_i64) {
        Some(seen) => Some(known.map_or(seen, |known| known.max(seen))),
        None => known,
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

    // `system`/`thinking_tokens` is deliberately NOT read here, although it is the only other line
    // carrying a token count. It measures what the model spent reasoning, not how full its window
    // is, and against a real stream the two are out by two orders of magnitude — 177 of thinking on
    // a turn carrying 48,733 of context. It is read by `live_from_stream` instead, under its own
    // name, where it says the thing it actually means.
    current
}

/// The fullest the stream ever got, rather than where it ended.
///
/// Written on top of `context_fill_from_line` instead of re-reading the JSON: there is one
/// definition of "what occupies the window" — `input_tokens + cache_read_input_tokens` — and it
/// lives there. Two copies would drift the day the CLI added a third counter, and drift silently.
///
/// The `None` in the call is deliberate: what is wanted is what THIS line says, not the running
/// total, so that the larger of the two can be chosen here.
pub(crate) fn context_peak_from_line(line: &str, current: Option<i64>) -> Option<i64> {
    match context_fill_from_line(line, None) {
        Some(fill) => Some(current.map_or(fill, |peak| peak.max(fill))),
        None => current,
    }
}

/// The environment this run's context window is expressed in, or nothing when it names none.
///
/// A function rather than two lines at the spawn site for one reason: the variable's NAME is the
/// part that fails silently. A typo in it leaves the CLI on its own default window, the daemon
/// still writes the number the window draws, and the only symptom is a conversation that compacts
/// at a size nobody asked for. Spelled once, here, where a test can read it back.
pub(crate) fn window_env(request: &RunRequest) -> Option<(&'static str, String)> {
    request
        .context_window
        .map(|window| ("CLAUDE_CODE_AUTO_COMPACT_WINDOW", window.to_string()))
}

/// The environment that takes background tasks away from a run nothing can wake, or nothing for a
/// run something can.
///
/// A background task reports back by waking the session that started it, and a run with no later
/// turn has no session left to wake: once its turn ends the CLI exits and kills the task with it.
/// Measured on run 900473 (CLI 2.1.263): the model launched the gate's build in the background,
/// ended its turn with "I'll wait for the background gate build (task `b84qqcytz`) to finish before
/// continuing — it'll notify automatically when done", and the stream closed on that task being
/// `killed`. The run was recorded `completed`, its work half done and uncommitted. A `sleep 20`
/// launched the same way reproduces it in thirteen seconds.
///
/// With `CLAUDE_CODE_DISABLE_BACKGROUND_TASKS=1` the CLI takes `run_in_background` out of the
/// `Bash` schema, so the same request is refused as an unexpected parameter and nothing is left
/// running when the turn ends — measured with the same prompt against the same CLI.
///
/// Only a steerable run with somewhere its later turns come from keeps them: its process outlives
/// the turn, which is what a task's notification needs. A steerable run with no channel closes its
/// stdin after the opening turn, and for this purpose is a headless run.
///
/// Set before `request.env` at the spawn site, like [`window_env`], so an explicit entry still wins.
pub(crate) fn background_env(request: &RunRequest) -> Option<(&'static str, &'static str)> {
    let can_be_woken = request.steerable && request.messages.is_some();
    (!can_be_woken).then_some(("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1"))
}

/// PURE: the background tasks this stream reports killed after its last answer, each named once.
///
/// The signature, read off run 900473's own stream:
///
/// ```text
/// {"type":"result","subtype":"success","stop_reason":"end_turn",...}
/// {"type":"system","subtype":"task_updated","task_id":"b84qqcytz","patch":{"status":"killed",...}}
/// {"type":"system","subtype":"task_notification","task_id":"b84qqcytz","status":"stopped",...}
/// ```
///
/// After the `result` and never before it: a task stopped mid-turn was stopped by the model, which
/// is a decision; one killed after the last answer was killed by the process ending under it.
///
/// The second line behind [`background_env`], not the first. With background tasks taken away this
/// finds nothing, and it exists for the day it would: a CLI that renames the variable would
/// otherwise bring back a run recorded `completed` with its work abandoned, and nothing saying so.
pub(crate) fn orphaned_background_tasks(stdout: &str) -> Vec<String> {
    let mut answered = false;
    let mut orphaned: Vec<String> = Vec::new();
    for line in stdout.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        match value.get("type").and_then(serde_json::Value::as_str) {
            Some("result") => {
                answered = true;
                continue;
            }
            Some("system") if answered => {}
            _ => continue,
        }
        let killed = match value.get("subtype").and_then(serde_json::Value::as_str) {
            Some("task_updated") => {
                value
                    .pointer("/patch/status")
                    .and_then(serde_json::Value::as_str)
                    == Some("killed")
            }
            Some("task_notification") => matches!(
                value.get("status").and_then(serde_json::Value::as_str),
                Some("stopped" | "killed")
            ),
            _ => false,
        };
        if killed
            && let Some(task) = value.get("task_id").and_then(serde_json::Value::as_str)
            && !orphaned.iter().any(|seen| seen == task)
        {
            orphaned.push(task.to_string());
        }
    }
    orphaned
}

/// Whether this line says the CLI compacted its own context.
///
/// The event, read off a real headless stream rather than inferred from the source:
///
/// ```text
/// {"type":"system","subtype":"status","status":"compacting","session_id":...}
/// {"type":"system","subtype":"status","status":null,"compact_result":"failed",
///  "compact_error":"too_few_groups","session_id":...}
/// ```
///
/// `status: "compacting"` is what is read, and the later `compact_result` deliberately is not. The
/// question this answers is "was the context summarised during this turn" — which is a thing the
/// transcript should say, because the alternative is a conversation that quietly got shorter — and
/// a compaction that began is the honest answer to it whether or not it finished. A `failed` result
/// means the context was left as it was; the turn still answered, and a mark that appeared and then
/// had to be taken back would be worse than one that says "this is where it summarised".
///
/// Sticky once true, like `larger` above: a turn can compact and then go on for many more lines,
/// and a flag recomputed from the last line alone would report only whatever happened to come last.
pub(crate) fn compacted_from_line(line: &str, current: bool) -> bool {
    if current {
        return true;
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
        return current;
    };
    value.get("type").and_then(serde_json::Value::as_str) == Some("system")
        && value.get("subtype").and_then(serde_json::Value::as_str) == Some("status")
        && value.get("status").and_then(serde_json::Value::as_str) == Some("compacting")
}

/// Usage reported by the final `result` event of a Claude `stream-json` transcript.
///
/// Missing fields stay unknown rather than becoming measured zeroes. `num_turns` belongs to the
/// result event itself; the token counts live under its `usage` object.
/// The processes kept alive between a conversation's turns, held so the kernel takes them down if
/// this daemon goes without getting the chance to.
///
/// A `Litter` and not a `TreeKiller`: the two are the same primitive with opposite flags, and this
/// is the one whose promise survives the daemon being killed rather than asked to stop.
static KEPT_ALIVE: std::sync::LazyLock<crate::process_tree::Litter> =
    std::sync::LazyLock::new(crate::process_tree::Litter::new);

/// One turn's own numbers, out of a process that may answer more than once.
///
/// `RunOutcome` describes a PROCESS — its exit code, its whole stdout, everything it spent. This
/// describes one answer inside it, which is the unit a conversation is billed and recorded by. The
/// two coincide exactly as long as a process serves one turn, and stop coinciding the moment one
/// serves two.
// `cost_usd` is an `f64`, which is not `Eq`, for the reason `RunOutcome` gives above its own derive.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TurnOutcome {
    pub session_id: Option<String>,
    /// What THIS turn added, never what the process has spent altogether.
    pub cost_usd: Option<f64>,
    pub usage: RunUsage,
    /// Whether the CLI summarised its context while producing THIS turn.
    ///
    /// Per turn and not per process, like the cost above and for the same reason: a process that
    /// serves six turns compacts during one of them, and a flag on the process would mark all six
    /// as the turn where the conversation got shorter.
    pub compacted: bool,
}

/// What one line of a live process's stream means to whoever is recording turns.
///
/// A process that serves one turn needs none of this — its stream IS the turn, and `RunOutcome`
/// describes it. A process that serves several needs somebody to say where one answer ends and the
/// next begins, because the CLI itself only says so in passing, with a `result` line that looks like
/// any other line until it is parsed.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnEvent {
    /// One line belonging to the turn that has not ended yet.
    Line(String),
    /// The turn that was in flight has ended, with its own numbers.
    Ended(TurnOutcome),
}

/// Splits one process's stdout into turns, a line at a time.
///
/// It exists so the rule lives in one place that a test can reach without a subprocess: `execute`
/// feeds it every line and reads back both what to forward and what the turn cost, and nothing else
/// in the daemon has to know that a `result` is a boundary or that the cost on it is cumulative.
pub(crate) struct TurnSplitter {
    spent: f64,
    /// Belongs to the turn IN FLIGHT, and is cleared when that turn ends.
    ///
    /// It is accumulated rather than read off the `result` line, because it is not on it: the CLI
    /// decides to compact before it answers. Cleared at the boundary and not merely overwritten,
    /// so a process serving six turns does not report the second one's compaction on the four
    /// that follow it.
    compacted: bool,
}

impl TurnSplitter {
    pub(crate) fn new() -> Self {
        Self {
            spent: 0.0,
            compacted: false,
        }
    }

    /// The events this line produces, in the order a consumer must see them.
    ///
    /// A `result` line produces BOTH — it is the last line of the turn it ends, and it carries the
    /// answer, so a consumer told the turn had ended before being given that line would close every
    /// turn one line short of what it said.
    pub(crate) fn line(&mut self, line: String) -> Vec<TurnEvent> {
        self.compacted = compacted_from_line(&line, self.compacted);
        match turn_from_result(&line, self.spent) {
            Some((mut turn, total)) => {
                self.spent = total;
                turn.compacted = std::mem::take(&mut self.compacted);
                vec![TurnEvent::Line(line), TurnEvent::Ended(turn)]
            }
            None => vec![TurnEvent::Line(line)],
        }
    }

    /// Everything the process has reported spending so far, which is what bills the PROCESS rather
    /// than any one turn inside it.
    pub(crate) fn spent(&self) -> f64 {
        self.spent
    }
}

/// What one `result` line added, given what the process has already reported spending — and the new
/// running total to carry into the next one. `None` for every line that does not end a turn.
///
/// The two halves are read differently ON PURPOSE, and the reason is measured rather than assumed.
/// Two turns fed down one stdin reported `total_cost_usd` 0.1046 and then 0.2024 — a running total
/// over the process — while `num_turns` read 1 on both and `usage` described only the turn that had
/// just ended. So the cost is differenced and nothing beside it is: differencing the counts would
/// produce a negative number the first time a turn used fewer tokens than the one before it, and
/// recording the cost verbatim would bill each turn for every turn that preceded it.
///
/// The session id is read here as well because a turn is where it becomes true: measured on the same
/// two turns, a live process keeps ONE session across all of them, which is what lets a conversation
/// still be resumed by it after the process is gone.
pub(crate) fn turn_from_result(line: &str, already_spent: f64) -> Option<(TurnOutcome, f64)> {
    let line = line.trim();
    let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
    if value.get("type").and_then(|kind| kind.as_str()) != Some("result") {
        return None;
    }
    // `cost_usd` as the fallback spelling for the same reason the stdout loop accepts both: older
    // CLI builds emit it, and a turn whose cost silently read `None` would be a free turn in the
    // ledger.
    let spent = value
        .get("total_cost_usd")
        .or_else(|| value.get("cost_usd"))
        .and_then(serde_json::Value::as_f64);
    let turn = TurnOutcome {
        session_id: value
            .get("session_id")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
        cost_usd: spent.map(|total| total - already_spent),
        usage: extract_usage(line),
        // The SPLITTER's to fill: the fact is not on the `result` line this function parses, and
        // inventing it here from nothing would be a quieter way of saying `false`.
        compacted: false,
    };
    // A result carrying no cost at all must not reset the total: the next turn would then be
    // differenced against zero and billed for the whole conversation.
    Some((turn, spent.unwrap_or(already_spent)))
}

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

/// PURE: what a stream that never reached a `result` still says it used.
///
/// [`extract_usage`] reads the `result` event, and a headless run emits exactly one, at the very
/// end. A run stopped before it — a turn ceiling, a progress deadline, a stream that broke — used to
/// write NULL in every column, whatever it had burned: run 900463 was stopped at its ceiling after
/// 125 responses and nearly ten million cache-read tokens, and recorded none of them.
///
/// Every `assistant` event carries its message's `usage`, and three of its fields are exact,
/// because the input side is settled before the model writes a word. Summed once per message — the
/// blocks of one message repeat the same figures, so summing events would count an answer once per
/// block — they matched the `result` of run 900473 to the token: 28 input, 1,023,866 cache read,
/// 68,996 cache creation.
///
/// `output_tokens` is not one of them and stays `None`. The figure on an `assistant` event is a
/// count taken while the message was still being written: the same run's messages summed to 40
/// against a `result` of 8,124. Written down, that would read as measured and be wrong by two
/// orders of magnitude; unknown is the honest value, as it is everywhere else in [`RunUsage`].
///
/// `num_turns` is counted by the same [`TurnCounter`] the ceiling reads, so a run stopped at its
/// ceiling records the number that stopped it. It is not the CLI's own `num_turns`, which counts
/// something else (16 against 14 messages on 900473) and never arrived here anyway.
///
/// And no cost. Nothing here prices tokens, and the budget already charges a run with no cost by
/// how long it ran (`budget::compute_spend`); a figure built from the input side alone would
/// displace that estimate with a smaller one.
pub(crate) fn usage_without_a_result(stdout: &str) -> RunUsage {
    let mut turns = TurnCounter::default();
    let mut usage = RunUsage::default();
    for line in stdout.lines() {
        if !turns.line(line) {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line.trim()) else {
            continue;
        };
        let Some(reported) = value
            .get("message")
            .and_then(|message| message.get("usage"))
        else {
            continue;
        };
        let add = |total: &mut Option<i64>, field: &str| {
            if let Some(tokens) = reported.get(field).and_then(serde_json::Value::as_i64) {
                *total = Some(total.unwrap_or(0).saturating_add(tokens));
            }
        };
        add(&mut usage.input_tokens, "input_tokens");
        add(&mut usage.cache_read_tokens, "cache_read_input_tokens");
        add(
            &mut usage.cache_creation_tokens,
            "cache_creation_input_tokens",
        );
    }
    usage.num_turns = (turns.count() > 0).then_some(turns.count());
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

    /// Runs a prompt while also reporting where one turn ends and the next begins.
    ///
    /// Only ever `Some` for a process meant to serve more than one turn. Everything else keeps the
    /// default and never learns that turns exist, which is right: for a process that answers once,
    /// the turn and the process are the same thing and `RunOutcome` already describes it.
    ///
    /// A channel rather than a return value because a caller has to act on a turn while the process
    /// it belongs to is still running — that is the entire point of keeping it running.
    async fn run_prompt_with_turns(
        &self,
        request: RunRequest,
        session_tx: UnboundedSender<String>,
        transcript: std::sync::Arc<std::sync::Mutex<String>>,
        context_fill: std::sync::Arc<std::sync::Mutex<Option<i64>>>,
        _turns: Option<UnboundedSender<TurnEvent>>,
    ) -> std::io::Result<RunOutcome> {
        self.run_prompt_with_context_fill(request, session_tx, transcript, context_fill)
            .await
    }

    /// The per-role model for a job node's stage, or `None` for the runner's own model.
    ///
    /// Defaulted so a runner with one model — every runner but the CLI one — answers `None` without
    /// having to say so, and so a caller that names no stage keeps today's behavior.
    fn model_for_stage(&self, _stage: Option<&str>) -> Option<String> {
        None
    }

    /// What this runner would itself write into the model's prompt for `request`, or `None` for a
    /// runner whose prompt this daemon does not author.
    ///
    /// **`None` is the default, and every runner but the Anthropic CLI one keeps it.** The four
    /// pieces `AuthoredPrompt` counts are the four `cli_args` puts on the command line, and they are
    /// facts about THAT argument vector: `OllamaRunner` sends no tool schemas and has no notion of a
    /// subagent, `CodexCliRunner` builds a different vector entirely, and the fakes build none. A
    /// default that answered `Some(…)` by measuring the request anyway would attribute this daemon's
    /// argv to processes that never received it, which is the one thing this accounting must not do.
    /// Silence is the honest answer, and the column behind it stays NULL — exactly as
    /// `runs.permission_mode` is NULL for anything that is not a chat turn.
    fn authored_prompt(
        &self,
        _request: &RunRequest,
    ) -> Option<crate::prompt_budget::AuthoredPrompt> {
        None
    }
}

/// The part of one CLI run's prompt that this daemon wrote, measured off the same values
/// [`cli_args`] puts on the command line.
///
/// Read this beside `cli_args` and not from anywhere else. Every field below names a flag written
/// there, under the same condition it is written under, so the two go wrong together or not at all;
/// a second source of truth for any of them is a number that quietly stops matching the day somebody
/// changes a flag.
///
/// **The schema block is priced from `mcp_config` being present.** A run with no `--mcp-config` is
/// offered no tools by this daemon, so its schema cost is a real zero rather than an unknown. A run
/// that has one pays for whatever its server announces, which is the whole tool list, read off the
/// same router that answers `list_tools`.
///
/// **A server that is ANNOUNCED is not a server whose schemas are SENT, and only the second is
/// charged.** When the CLI keeps its `ToolSearch` built-in it advertises MCP tools by NAME and
/// fetches a schema only when the model asks for one, so the block this function prices is not in
/// the prompt at all and the run owes nothing for it. When `ToolSearch` is denied the CLI cannot
/// defer, ships every schema, and the whole announcement is the right price. [`schemas_are_deferred`]
/// is where that question is asked, and where the measurements are written down — this rule was
/// taken off a live CLI, not reasoned from its documentation.
///
/// Not a display nicety. `runs::RunStatusResponse::with_prompt_budget` derives "the CLI's own" by
/// SUBTRACTING this estimate from the reported prompt total, so charging the authored side for a
/// ~10,250-character schema block that was never sent understates the residual by exactly as much.
/// And the common case is the deferring one: a shell chat turn is `ToolPolicy::Unrestricted`
/// (`assistant::tool_policy_for`), so this arm is the one an ordinary turn takes.
pub(crate) fn authored_prompt(request: &RunRequest) -> crate::prompt_budget::AuthoredPrompt {
    crate::prompt_budget::AuthoredPrompt {
        schema_chars: match request.mcp_config {
            None => 0,
            // Announced but never sent: charged nothing, because nothing was read. This zero is a
            // different fact from the one above it, and `AuthoredPrompt::schema_chars` is where the
            // two are told apart for whoever reads the stored number.
            Some(_) if schemas_are_deferred(request) => 0,
            Some(_) => crate::mcp_tools::NucleosTools::advertised_schema_chars(),
        },
        // Only counted when the flag is actually written. `Some("")` is not a state any caller
        // builds, but counting an absent value as zero and a present one by its length is what keeps
        // this in step with the `if let` in `cli_args`.
        system_prompt_chars: request.append_system_prompt.as_ref().map_or(0, String::len),
        // Empty writes NO flag — not `{}` — so an empty helper set costs nothing, and calling
        // `agents_json` on it would charge two characters for a flag that was never written.
        agents_chars: if request.agents.is_empty() {
            0
        } else {
            agents_json(&request.agents).len()
        },
        // Charged whether it travels as a positional argument or on stdin. `steerable` decides which
        // of the two, and the model reads the same characters either way.
        prompt_chars: request.prompt.len(),
    }
}

/// Whether this run's MCP tool schemas are DEFERRED — announced by name, fetched only if the model
/// asks — rather than shipped whole inside the prompt.
///
/// **Asked of `denied_tools`, and never of the policy.** The expression below is the one `cli_args`
/// writes onto `--disallowedTools`, so the price and the flag go wrong together or not at all. A
/// `match` on `ToolPolicy` variants would answer the same for today's two policies and still be the
/// wrong contract: a run that is `Unrestricted` and names `"ToolSearch"` in its own `denied_tools`
/// has had the deferral taken away from it by name, ships every schema, and a variant match would
/// charge it nothing.
///
/// Measured on 2026-09-07 against CLI 2.1.263 — one prompt, one flag different per arm, input
/// tokens as the CLI reported them:
///
/// | offered | tools | input tokens |
/// |---|---|---|
/// | nothing at all | 0 | 3,612 |
/// | 48 nucleos tools, every built-in denied | 48 | 15,837 |
/// | 32 built-ins, no MCP server | 32 | 29,756 |
/// | 32 built-ins + the same 48 nucleos tools | 80 | 30,606 |
///
/// The same 48 tools cost 12,225 tokens in one row and 850 in the other. In the shipped regime
/// `advertised_schema_chars / 4` is right to within 9% (10,250 estimated against ~11,160
/// attributable); in the deferred one it overstates the truth by more than an order of magnitude.
///
/// **`ToolSearch` is the single variable, and that was tested directly rather than inferred.** The
/// rows above differ by a whole built-in set, which leaves open the rival explanation that the CLI
/// defers once some TOOL COUNT is passed. A further arm denied every built-in EXCEPT `ToolSearch`,
/// against the same 48-tool server: 49 tools, 5,578 input tokens — deferred — where 48 tools with
/// no `ToolSearch` cost 15,837 and shipped. A count threshold cannot make 49 defer while 48 ships.
/// `"ToolSearch"` is itself on `BUILTIN_TOOLS`, which is how denying the built-ins denies it.
///
/// `ToolPolicy::None` answers `["*"]`, which names no tool literally, so it reads as deferred and is
/// charged nothing. That is the right answer by a different road — such a run is advertised NO tools
/// at all, so there is no schema block in its prompt either — and it is unreachable regardless:
/// every `None`-policy request in this codebase pairs the policy with `mcp_config: None`
/// (`council::run_cloud_seat` makes the config `with_tools.then(…)`, `map_intent` sets neither, and
/// `create_run_inner` never sets a config at all). It is left to the literal question above rather
/// than special-cased, because the moment this stops being one reading of what `cli_args` writes, it
/// starts being a second source of truth.
fn schemas_are_deferred(request: &RunRequest) -> bool {
    // Deferral is a CAPABILITY, so the question is whether the run still has it: `ToolSearch` on
    // the denied list is the CLI being unable to fetch a schema on demand, which is the shipped
    // regime. Reading the same list the flag is built from is the whole point of asking here.
    !denied_tools(&request.tool_policy, &request.denied_tools)
        .iter()
        .any(|name| name == "ToolSearch")
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
pub(crate) const OLLAMA_EXCHANGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

impl OllamaChat {
    /// Builds from a client the caller already owns, and is the only constructor: every
    /// `OllamaChat` in the daemon comes from `assistants::Assistants::local_chat`, so every one of
    /// them shares the ONE `reqwest::Client` the local route was built with in
    /// `assistants::ConfiguredAssistants::new` — which is also where the reason that client has a
    /// timeout now lives. There was a `new` beside this that built its own; its last caller was
    /// `team.rs`, and a second constructor is a second way for a reader to reach Ollama without
    /// asking the factory which engine is configured.
    pub fn with_client(client: reqwest::Client, base_url: String, model: String) -> Self {
        Self {
            client,
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
                compacted: false,
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
            compacted: false,
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

    /// The one runner that authors a prompt this daemon can price, because it is the one whose
    /// argument vector `cli_args` builds. See the trait's default for why nobody else answers.
    fn authored_prompt(
        &self,
        request: &RunRequest,
    ) -> Option<crate::prompt_budget::AuthoredPrompt> {
        Some(authored_prompt(request))
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
        request: RunRequest,
        session_tx: UnboundedSender<String>,
        transcript: std::sync::Arc<std::sync::Mutex<String>>,
        context_fill: std::sync::Arc<std::sync::Mutex<Option<i64>>>,
    ) -> std::io::Result<RunOutcome> {
        // Nobody listening for turns, which is every caller but a conversation keeping its process.
        self.run_prompt_with_turns(request, session_tx, transcript, context_fill, None)
            .await
    }

    async fn run_prompt_with_turns(
        &self,
        mut request: RunRequest,
        session_tx: UnboundedSender<String>,
        transcript: std::sync::Arc<std::sync::Mutex<String>>,
        context_fill: std::sync::Arc<std::sync::Mutex<Option<i64>>>,
        turn_events: Option<UnboundedSender<TurnEvent>>,
    ) -> std::io::Result<RunOutcome> {
        // The Claude Code CLI binary. Overridable via `NUCLEOS_CLAUDE_BIN` because on Windows the
        // npm-installed `claude` is a `.cmd` shim that Rust's `Command` can't spawn by name — the
        // daemon points this at the real `claude.exe`. Defaults to `claude` where it's on PATH.
        let claude_bin =
            std::env::var("NUCLEOS_CLAUDE_BIN").unwrap_or_else(|_| "claude".to_string());
        let mut cmd = Command::new(&claude_bin);
        cmd.args(cli_args(&request, &self.model));
        // Before `request.env` and not after, so an explicit entry still wins. That is what a test
        // needs to force a window the CLI would otherwise clamp away, and it costs nothing here:
        // no caller sets both.
        if let Some((name, value)) = window_env(&request) {
            cmd.env(name, value);
        }
        // Read now, while `request.messages` is still there to be asked about: the steering task
        // takes it once the process is running.
        let background_off = background_env(&request);
        if let Some((name, value)) = background_off {
            cmd.env(name, value);
        }
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

        // A process meant to outlive its TURN must not outlive the DAEMON.
        //
        // `TreeKiller` above covers being dropped, which is a thing that happens in a program that
        // is still running. It does nothing for `TerminateProcess` — what Task Manager,
        // `Stop-Process` and a crash all do — which runs no destructor and leaves every child
        // alive. `process_tree` records what that costs, measured this month: two days of daemon
        // restarts left 31 orphaned sidecars, each holding the loopback port its own replacement
        // then died trying to bind.
        //
        // A one-turn run is already bounded by its turn and is not enrolled. A CLI held idle
        // between a conversation's turns is exactly the shape of thing that incident was about.
        if turn_events.is_some()
            && let Some(pid) = child.id()
        {
            KEPT_ALIVE.adopt(pid);
        }

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
            let opening = user_message_line(&request.prompt, &request.images);
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
                while let Some(turn) = messages.recv().await {
                    // Its own pictures, not none. The opening turn is no longer the only one
                    // anybody attaches anything to: a conversation that keeps its process makes
                    // every turn after the first arrive here.
                    if stdin
                        .write_all(user_message_line(&turn.text, &turn.images).as_bytes())
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
        // Where one answer ends and the next begins. Held across the whole stream because that is
        // the only place the running total lives.
        let mut splitter = TurnSplitter::new();
        let mut usage = RunUsage::default();
        let mut turn_ended = false;
        let mut running_context_fill: Option<i64> = None;
        let mut compacted = false;

        let mut post_launch_error: Option<std::io::Error> = None;
        let mut policy_violation: Option<String> = None;
        let mut init_seen = false;
        let mut progress_timeout_elapsed: Option<Duration> = None;
        let mut turns = TurnCounter::default();
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
            // Read on every line and NOT only near the end: the CLI decides to compact before it
            // answers. A run cut short here — a timeout, a turn ceiling — has still had its context
            // summarised, and the record should say so.
            compacted = compacted_from_line(&line, compacted);
            // After the line is accumulated and mirrored, never before: a run stopped here still has
            // to leave the transcript of the turn that stopped it, or the evidence for why it was
            // stopped is the one thing missing from the record.
            turns.line(&line);
            if over_turn_ceiling(turns.count(), request.max_turns) {
                turns_exceeded = Some(turns.count());
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
                // Every `result` is the end of a TURN, which is the same thing as the end of the
                // process only while a process serves one. `TurnSplitter` is where that distinction
                // is written down — the cost a result carries is a running total and the counts
                // beside it are not — so it is read here even when nobody is listening for turns,
                // rather than kept as a second account that can drift from this one.
                //
                // What this loop reports is unchanged: `cost_usd` is still the process's whole bill,
                // and `usage` still describes the turn that ended last.
                for event in splitter.line(line.clone()) {
                    if let TurnEvent::Ended(turn) = &event {
                        turn_ended = true;
                        usage = turn.usage;
                        if turn.cost_usd.is_some() {
                            cost_usd = Some(splitter.spent());
                        }
                    }
                    // Best-effort, like `session_tx` above it: a listener that has gone away is a
                    // conversation that stopped caring, and this stream has a process to keep
                    // draining either way.
                    if let Some(turn_events) = &turn_events {
                        let _ = turn_events.send(event);
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
        // Only for a run nothing can wake, which is the run background tasks were taken from: a
        // conversation that keeps its process is still there when its task finishes.
        let orphaned = if background_off.is_some() {
            orphaned_background_tasks(&stdout_acc)
        } else {
            Vec::new()
        };
        if !orphaned.is_empty() {
            if !stderr_str.is_empty() && !stderr_str.ends_with('\n') {
                stderr_str.push('\n');
            }
            stderr_str.push_str(&format!(
                "nucleos: this run ended its turn with background task(s) {} still running, and \
                 they died with the process; nothing can deliver their result to a run with no \
                 later turn, so the work they were doing never finished\n",
                orphaned.join(", ")
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
            // The CLI's zero is a claim about the turn, and says nothing about the work it left
            // running when the turn ended.
            (None, None, None, None) if !orphaned.is_empty() => -1,
            (None, None, None, None) => status.and_then(|status| status.code()).unwrap_or(-1),
        };

        // Only when no turn ended: a `result` is the CLI's own account and is never second-guessed,
        // and a stream without one still said, message by message, what it read.
        if !turn_ended {
            usage = usage_without_a_result(&stdout_acc);
        }

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
            compacted,
        })
    }
}

/// What `run_prompt` prepared before building argv: MCP overrides, staged image paths and the
/// sandbox the runner pins.
#[derive(Debug, Default)]
pub(crate) struct CodexStaged {
    pub mcp_overrides: Vec<String>,
    pub images: Vec<std::path::PathBuf>,
    /// The sandbox this launch pins, or `None` to leave Codex's own resolution.
    pub sandbox_mode: Option<&'static str>,
}

/// Translates the daemon's stdio MCP configuration into Codex overrides.
/// `codex exec` runs with approval policy `never`, so Codex immediately declines an MCP tool call
/// that needs confirmation ("user cancelled MCP tool call" on 0.144.4). `approve` pre-approves
/// the daemon's own MCP servers, as the Claude path does with `--allowedTools mcp__nucleos__*`.
pub(crate) fn codex_mcp_overrides(
    config: &serde_json::Value,
    env_names: &[String],
) -> Result<Vec<String>, String> {
    let servers = config
        .get("mcpServers")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "mcp_config must contain an mcpServers object".to_string())?;
    let mut names: Vec<&String> = servers.keys().collect();
    names.sort();

    let mut overrides = Vec::new();
    for name in names {
        if !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            return Err(format!(
                "mcp_config server name {name:?} cannot be expressed"
            ));
        }
        let server = servers
            .get(name)
            .and_then(serde_json::Value::as_object)
            .ok_or_else(|| format!("mcp_config server {name:?} must be an object"))?;
        if !matches!(
            server.get("type").and_then(serde_json::Value::as_str),
            None | Some("stdio")
        ) {
            return Err(format!("mcp_config server {name:?} must be stdio"));
        }
        let command = server
            .get("command")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| format!("mcp_config server {name:?} must have a string command"))?;
        let args = match server.get("args") {
            None => Vec::new(),
            Some(args) => args
                .as_array()
                .and_then(|args| {
                    args.iter()
                        .map(|arg| arg.as_str().map(str::to_string))
                        .collect()
                })
                .ok_or_else(|| format!("mcp_config server {name:?} args must be a string array"))?,
        };
        overrides.push(format!(
            "mcp_servers.{name}.command={}",
            serde_json::to_string(command).expect("serializing a string cannot fail")
        ));
        overrides.push(format!(
            "mcp_servers.{name}.args={}",
            serde_json::to_string(&args).expect("serializing strings cannot fail")
        ));
        if !env_names.is_empty() {
            overrides.push(format!(
                "mcp_servers.{name}.env_vars={}",
                serde_json::to_string(env_names).expect("serializing strings cannot fail")
            ));
        }
        overrides.push(format!(
            "mcp_servers.{name}.default_tools_approval_mode={}",
            serde_json::to_string("approve").expect("serializing a string cannot fail")
        ));
    }
    Ok(overrides)
}

/// Reads the thread id from Codex's thread-started event.
pub(crate) fn codex_thread_id(line: &str) -> Option<String> {
    let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
    (value.get("type").and_then(serde_json::Value::as_str) == Some("thread.started"))
        .then(|| value.get("thread_id").and_then(serde_json::Value::as_str))
        .flatten()
        .map(str::to_string)
}

/// Decodes opening-turn images into files readable by the Codex CLI.
pub(crate) fn stage_codex_images(
    dir: &std::path::Path,
    stem: &str,
    images: &[Attachment],
) -> std::io::Result<Vec<std::path::PathBuf>> {
    images
        .iter()
        .enumerate()
        .map(|(index, image)| {
            let extension = match image.media_type.as_str() {
                "image/png" => "png",
                "image/jpeg" => "jpg",
                "image/gif" => "gif",
                "image/webp" => "webp",
                _ => {
                    return Err(std::io::Error::other(
                        "codex exec cannot honour images: unsupported media type",
                    ));
                }
            };
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&image.data)
                .map_err(|error| {
                    std::io::Error::other(format!("codex exec cannot honour images: {error}"))
                })?;
            let path = dir.join(format!("{stem}-{index}.{extension}"));
            std::fs::write(&path, bytes)?;
            Ok(path)
        })
        .collect()
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
pub(crate) fn codex_cli_args(
    request: &RunRequest,
    model: &str,
    staged: &CodexStaged,
) -> Result<Vec<String>, String> {
    if request.fork_session {
        return Err(
            "codex exec cannot honour fork_session: it has no way to branch an existing session"
                .to_string(),
        );
    }
    if request.steerable && request.messages.is_some() {
        return Err(
            "codex exec cannot honour steerable: it has no stdin a later turn can arrive on"
                .to_string(),
        );
    }

    if !request.images.is_empty() && staged.images.len() != request.images.len() {
        return Err(
            "codex exec cannot honour images: staging did not produce every image".to_string(),
        );
    }

    let mut args = vec![
        // The non-interactive subcommand leads the vector; anything else opens a TUI, and a
        // daemon-spawned run has no terminal for one.
        "exec".to_string(),
        // A run works inside a worktree or a plain folder, and the CLI otherwise refuses to start
        // over the shape of that directory — a refusal about the ground rather than about the work.
        if request.resume_session_id.is_some() {
            "resume".to_string()
        } else {
            "--json".to_string()
        },
    ];
    if request.resume_session_id.is_some() {
        args.push("--json".to_string());
    }
    args.push("--skip-git-repo-check".to_string());
    for image in &staged.images {
        args.push("-i".to_string());
        args.push(image.to_string_lossy().into_owned());
    }
    args.extend(["-m".to_string(), model.to_string()]);
    if request.resume_session_id.is_none() {
        // `-C` is the only thing keeping a fresh run inside its project.
        if let Some(dir) = &request.cwd {
            args.push("-C".to_string());
            args.push(dir.to_string_lossy().into_owned());
        }
        for dir in &request.add_dirs {
            args.push("--add-dir".to_string());
            args.push(dir.to_string_lossy().into_owned());
        }
    } else if !request.add_dirs.is_empty() {
        let roots: Vec<String> = request
            .add_dirs
            .iter()
            .map(|dir| dir.to_string_lossy().into_owned())
            .collect();
        args.extend([
            "-c".to_string(),
            format!(
                "sandbox_workspace_write.writable_roots={}",
                serde_json::to_string(&roots).expect("serializing paths cannot fail")
            ),
        ]);
    }
    // Use `-c`, not `-s`, because `codex exec resume` has no `-s` flag.
    // It beats `sandbox_mode` in the user's `~/.codex/config.toml`, which exec's default does not.
    if let Some(mode) = staged.sandbox_mode {
        args.extend([
            "-c".to_string(),
            format!(
                "sandbox_mode={}",
                serde_json::to_string(mode).expect("serializing a string cannot fail")
            ),
        ]);
    }
    if let Some(effort) = &request.effort {
        args.extend([
            "-c".to_string(),
            format!(
                "model_reasoning_effort={}",
                serde_json::to_string(effort).expect("serializing a string cannot fail")
            ),
        ]);
    }
    for override_value in &staged.mcp_overrides {
        args.push("-c".to_string());
        args.push(override_value.clone());
    }
    if let Some(session_id) = &request.resume_session_id {
        args.push(session_id.clone());
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
    /// The sandbox every launch of this runner pins; `None` leaves Codex's own resolution (the
    /// user's config, else exec's read-only default).
    pub sandbox_mode: Option<&'static str>,
}

impl CodexCliRunner {
    /// Builds the runner used for a chat turn answered by Codex (`Assistants::cli_runner`).
    /// The owner's 2026-09-14 decision pins chat turns read-only.
    /// KNOWN LIMITATION: a daemon whose primary runner is Codex uses its run runner through
    /// `assistant::runner_for_turn`, so it keeps the user's Codex config as before this branch.
    pub fn for_chat(model: String) -> Self {
        Self {
            model,
            sandbox_mode: Some("read-only"),
        }
    }
}

#[async_trait]
impl CommandRunner for CodexCliRunner {
    async fn run_prompt(
        &self,
        request: RunRequest,
        session_tx: UnboundedSender<String>,
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
        // `Permission` is the reason this is a refusal rather than a log line, and every rung above
        // `Default` is refused. `Plan` is how a run is made unable to act — a catch-up run,
        // recovering a schedule the machine slept through, is forced into it precisely because
        // nobody chose for it to run NOW — so a runner that ignores it turns a deliberately
        // restrained run into an unrestrained one, in the one case where the operator was not
        // watching. `Bypass` fails the opposite way and worse: it is the CLI's permission barrier
        // standing down on the strength of a `PreToolUse` hook, which `codex exec` has no notion of,
        // so honouring it means running unbarriered where the daemon believes a second barrier took
        // over. `AcceptEdits` is a per-conversation control a person set in the window, and dropping
        // it silently leaves the row claiming something the run never had.
        //
        // `Default` alone passes, and passes by writing nothing: it is the rung that asks the launch
        // surface for no elevation at all, which is what `codex exec` already does.
        if request.permission != Permission::Default {
            return Err(std::io::Error::other(format!(
                "codex exec cannot honour Permission::{:?}: it has no permission mode that says this",
                request.permission
            )));
        }
        // MCP config is honoured through `codex_mcp_overrides`, which translates it into
        // `-c mcp_servers.<name>...` overrides.
        // The daemon's own servers are pre-approved (`default_tools_approval_mode = "approve"`).
        // Servers in the user's `~/.codex/config.toml` still load with that file's approval because
        // `codex exec` has no counterpart to the Claude CLI's `--strict-mcp-config`.
        //
        // The barrier this used to stand down is `Permission::Bypass`'s to stand down now, and the
        // guard above refuses that. What is left here is a BELIEF and it is still worth refusing:
        // this flag says the daemon has decided a `PreToolUse` classifier governs what this run may
        // call — and `codex exec` knows nothing of that mechanism, so no classifier will run.
        // Dropped silently it would not weaken THIS launch, but it would leave the daemon believing
        // a run is governed by something that never ran, which is the state every guard in this
        // block exists to refuse.
        if request.classifier_governs_tools {
            return Err(std::io::Error::other(
                "codex exec cannot honour classifier_governs_tools: it has no PreToolUse hook, so nothing would do the governing the daemon believes is happening",
            ));
        }
        // Every per-conversation control this launch surface has no counterpart for, refused in
        // one place and named individually so the message says which one.
        //
        // These arrived with 0110–0113 and each was accepted and dropped here — the outcome the
        // block above exists to prevent. They are not one kind of thing, and refusing them together
        // is still right, because they fail the same way: the chat row says a conversation is
        // pinned to a model, capped at a dollar a turn, barred from `Bash` and carrying standing
        // instructions, the window draws all four, and none of them reached the process.
        //
        // `denied_tools` is the sharpest of them. It passes the `ToolPolicy` guard above — a
        // conversation can be `Unrestricted` and still have barred a tool for itself — so without
        // this it would be a restriction somebody set, saw drawn back at them, and never had.
        for (asked, control) in [
            (!request.fallback_model.is_empty(), "fallback_model"),
            (request.max_budget_usd.is_some(), "max_budget_usd"),
            (!request.agents.is_empty(), "agents"),
            (
                request.append_system_prompt.is_some(),
                "append_system_prompt",
            ),
            (!request.denied_tools.is_empty(), "denied_tools"),
        ] {
            if asked {
                return Err(std::io::Error::other(format!(
                    "codex exec cannot honour {control}: it has no counterpart for it, and dropping one would leave the chat row claiming a control the run never had"
                )));
            }
        }
        // KNOWN LIMITATION, left un-refused on purpose, beside `session_name` below:
        // `context_window` is not honoured here, and it is the one control on this list that is
        // safe to lose. It is exported as `CLAUDE_CODE_AUTO_COMPACT_WINDOW`, which is a Claude
        // Code environment variable that `codex exec` reads no meaning into; a run that loses it
        // compacts on whatever schedule Codex has of its own, which is the schedule every run on
        // this path has always had. Nothing is loosened and no record claims otherwise — the
        // window a chat row names is drawn from the row, and the row is still true about the
        // Claude path it was written for.

        // KNOWN LIMITATION, left un-refused on purpose, beside `resume_session_id` below:
        // `session_name` is not honoured here. It reaches the Claude CLI's `--resume` picker and
        // nothing else — no decision anywhere depends on it, and no record claims it was applied —
        // so a run that loses it is a run with a nameless session, which is what every run on this
        // path has always had.

        // Partial messages are accepted and degrade: Codex's stream has no partial-message events,
        // so the reply arrives when the turn completes.
        // Resume is honoured through `codex exec resume`, which has its own argument shape.
        //
        // The args are built before anything is spawned so a refusal reaches the caller as the `Err` that means
        // the CLI never ran — which `runs::spawn_run` reads as "a retry cannot double-apply a
        // mutation", and that is precisely true of a launch that did not happen.
        // `codex_cli_args` honours MCP config, effort, and extra directories; partial messages
        // degrade because the reply arrives when the turn completes.
        let mcp_overrides = match &request.mcp_config {
            Some(path) => {
                let contents = std::fs::read(path).map_err(|error| {
                    std::io::Error::other(format!("codex exec cannot honour mcp_config: {error}"))
                })?;
                let config = serde_json::from_slice(&contents).map_err(|error| {
                    std::io::Error::other(format!("codex exec cannot honour mcp_config: {error}"))
                })?;
                let mut env_names: Vec<String> =
                    request.env.iter().map(|(name, _)| name.clone()).collect();
                env_names.sort();
                codex_mcp_overrides(&config, &env_names).map_err(|why| {
                    std::io::Error::other(format!("codex exec cannot honour mcp_config: {why}"))
                })?
            }
            None => Vec::new(),
        };
        struct StagedFiles(Vec<PathBuf>);
        impl Drop for StagedFiles {
            fn drop(&mut self) {
                for path in &self.0 {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        let staged_files = if request.images.is_empty() {
            StagedFiles(Vec::new())
        } else {
            let stem = format!(
                "nucleos-codex-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            );
            StagedFiles(stage_codex_images(
                std::env::temp_dir().as_path(),
                &stem,
                &request.images,
            )?)
        };
        let staged = CodexStaged {
            mcp_overrides,
            images: staged_files.0.clone(),
            sandbox_mode: self.sandbox_mode,
        };
        let args = codex_cli_args(
            &request,
            request.model.as_deref().unwrap_or(&self.model),
            &staged,
        )
        .map_err(std::io::Error::other)?;

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
        let mut turns = TurnCounter::default();
        let mut turns_exceeded: Option<i64> = None;
        let mut thread_id = None;

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
            if thread_id.is_none() {
                thread_id = codex_thread_id(&line);
                if let Some(id) = &thread_id {
                    let _ = session_tx.send(id.clone());
                }
            }
            // The same brake as the Claude body above, counting `turn.completed` instead of
            // `assistant` — `TurnCounter` knows both, so this path cannot drift out of step
            // with the other by being edited on its own.
            turns.line(&line);
            if over_turn_ceiling(turns.count(), request.max_turns) {
                turns_exceeded = Some(turns.count());
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
            // The thread id from `thread.started` when the stream carried one, else the caller's id.
            session_id: thread_id.or_else(|| {
                request
                    .session_id
                    .clone()
                    .or_else(|| request.resume_session_id.clone())
            }),
            // This tool reports no price. Unknown, not free — `budget.rs` bills against this field,
            // and a `Some(0.0)` would make every run on this path look like it spent nothing.
            cost_usd: None,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_creation_tokens: usage.cache_creation_tokens,
            num_turns: usage.num_turns,
            compacted: false,
        })
    }
}

/// What a run was launched WITH, as opposed to what it went on to do.
///
/// Its own type rather than four loose fields, because the four answer one question together --
/// does this run start with a past?
#[cfg(test)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub prompt: String,
    pub resume_session_id: Option<String>,
    pub session_id: Option<String>,
    pub fork_session: bool,
}

/// The test double for `CommandRunner`. `#[cfg(test)]` because every user of it is a test — building
/// it into the daemon would ship a runner that can fake a run's outcome.
#[cfg(test)]
#[derive(Default)]
pub struct FakeCommandRunner {
    pub canned: std::sync::Mutex<Option<RunOutcome>>,
    // Set by Task 4's cancellation/timeout tests to simulate a slow/hung run.
    pub delay: std::sync::Mutex<Option<std::time::Duration>>,
    pub last_permission: std::sync::Mutex<Option<Permission>>,
    /// The prompt the launch was handed. Recorded because a turn's prompt is not always the text
    /// the person typed — a launcher may prepend context to it — so what the CLI actually
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
    /// The pictures the launch was handed. Recorded because bytes can only travel the stdin path,
    /// so a run given images and not `steerable` drops them without a word — a failure invisible
    /// from everywhere except here.
    pub last_images: std::sync::Mutex<Option<Vec<Attachment>>>,
    /// Whether the launch handed the run the classifier's permission surface instead of the CLI's.
    /// Recorded for the same reason `last_tool_policy` is: it decides what a run CAN do.
    pub last_classifier_governs_tools: std::sync::Mutex<Option<bool>>,
    /// What the CLI was handed in its environment. Recorded because a run with a Bash tool can read
    /// its own environment, so which key lands here is a safety property and not a detail.
    pub last_env: std::sync::Mutex<Option<Vec<(String, String)>>>,
    /// Which model and effort the launch was handed. Recorded because there is nowhere else to
    /// observe them: `cli_args` proves the flags are BUILT from a request, and this proves the
    /// request a conversation produces carries what that conversation chose. Between the two there
    /// used to be a gap wide enough for `model: None` to sit in unnoticed for the life of the
    /// feature.
    pub last_model: std::sync::Mutex<Option<Option<String>>>,
    pub last_effort: std::sync::Mutex<Option<Option<String>>>,
    /// Recorded for the reason `last_model` is: `cli_args` proves the flags are built out of a
    /// request, and this proves the request a conversation produces carries what it was told.
    pub last_fallback_model: std::sync::Mutex<Option<Vec<String>>>,
    pub last_add_dirs: std::sync::Mutex<Option<Vec<PathBuf>>>,
    pub last_max_budget_usd: std::sync::Mutex<Option<Option<f64>>>,
    pub last_agents: std::sync::Mutex<Option<Vec<Subagent>>>,
    pub last_append_system_prompt: std::sync::Mutex<Option<Option<String>>>,
    pub last_denied_tools: std::sync::Mutex<Option<Vec<String>>>,
    pub last_session_name: std::sync::Mutex<Option<Option<String>>>,
    /// Test-only: whether this double answers `authored_prompt` the way `ClaudeCliRunner` does.
    ///
    /// `false` by default, and that default is the honest one: a fake builds no argument vector, so
    /// the trait's own reasoning applies to it unchanged — it leaves `runs.authored_prompt_chars`
    /// NULL, exactly as the local model and the Codex CLI do, and every existing test here wants
    /// precisely that.
    ///
    /// Turned on by the handful of tests asking a question about a LAUNCH SITE rather than about a
    /// runner: does this launcher record what it wrote into the prompt, and does it record the right
    /// figure? There is no other way to ask it. The one runner that answers `Some(_)` is the one
    /// that spawns a real `claude`, so a test wanting the recording to happen would have to spawn a
    /// process — and what it would then be testing is the CLI's presence on the machine, not the
    /// call this daemon makes. The arithmetic itself is not on trial here; it is pinned against the
    /// real function in `runner`'s own tests.
    pub prices_its_prompt: bool,
    /// Test-only: return an `Err` (simulated launch failure — no work done) for the first N calls.
    pub fail_times: std::sync::Mutex<u32>,
    /// Test-only: count of run_prompt invocations.
    pub calls: std::sync::Mutex<u32>,
    /// Test-only: how the last call was launched.
    ///
    /// `runs.rs`'s context handoff is why this exists. Its successor was launched
    /// `--resume <predecessor> --fork-session`, which copies a conversation rather than ending one,
    /// and nothing here could see it: this fake recorded the environment and the outcome, never the
    /// session shape, so a run inheriting everything looked exactly like one inheriting nothing. It
    /// took a live CLI to notice.
    pub last_launch: std::sync::Mutex<Option<Launch>>,
    /// Test-only: whether a living process was STOPPED rather than allowed to end.
    ///
    /// Set by a guard the fake's own future holds, and disarmed just before that future returns —
    /// so it says "this was dropped mid-flight" and not merely "this finished". Nothing else can
    /// tell those apart from outside, and the difference is the whole of what a cancel has to do.
    pub stopped_early: std::sync::Arc<std::sync::Mutex<bool>>,
    /// Test-only: the turns written to a living process's stdin after the one it was launched with.
    ///
    /// Recorded because a later turn's pictures can only be observed here: they travel down a
    /// channel into a task that writes them to a pipe, and a run given them and not steerable drops
    /// them without a word.
    pub later_turns: std::sync::Mutex<Vec<LaterTurn>>,
    /// Test-only: the queue a plan node writes, taken by the first call that is given a handoff
    /// directory.
    ///
    /// Written into the directory named by `NUCLEOS_JOB_ARTIFACTS`, exactly where a real plan node
    /// would put it — so a test of the job chain goes through the env plumbing and reads the queue
    /// off disk, instead of reaching around both to seed a queue the daemon never saw. Taken rather
    /// than copied, because only the first node of a job plans: an implement node that rewrote the
    /// queue it is working from is a fiction no real run can produce.
    pub plan_to_write: std::sync::Mutex<Option<String>>,
    /// Test-only: a scripted agent. `(marker, path, contents)` — a run whose PROMPT contains the
    /// marker writes `contents` to `path` inside the checkout it was handed.
    ///
    /// **Keyed on the prompt and not on the order of calls**, which is the whole point. Two items of
    /// one batch start as two spawned tasks and reach this in whatever order the runtime picks, so a
    /// queue taken one entry per call would hand item 3's file to item 1 about half the time, and
    /// the test built on it would be measuring the scheduler's mood. The prompt carries the item's
    /// description, which is the one thing that identifies the item from in here.
    ///
    /// It writes and does NOT commit, because that is what a real implement node does: the prompt
    /// tells it to leave the tree uncommitted and `merge_item` is what commits. A double that
    /// committed would put a fixture back to asserting something no code does.
    pub writes: std::sync::Mutex<Vec<(String, String, String)>>,
}

#[cfg(test)]
#[async_trait]
impl CommandRunner for FakeCommandRunner {
    /// `None` unless a test has explicitly asked this double to stand in for the CLI runner here —
    /// see [`FakeCommandRunner::prices_its_prompt`] for why that is opt-in and what it is for.
    ///
    /// When it is asked, it answers through the very same free function `ClaudeCliRunner` calls, so
    /// the double cannot come to price a request differently from the runner it is standing in for.
    /// A second copy of that arithmetic living in the test double would be a test that keeps passing
    /// after the production rule changes underneath it.
    fn authored_prompt(
        &self,
        request: &RunRequest,
    ) -> Option<crate::prompt_budget::AuthoredPrompt> {
        self.prices_its_prompt.then(|| authored_prompt(request))
    }

    /// The live-process door, so a conversation that keeps its CLI is exercised in tests rather than
    /// only in production.
    ///
    /// Without this the default would delegate to `run_prompt`, which sends no turn events at all —
    /// and a rooted chat turn would sit waiting for a boundary that never came, fail, and still let
    /// every existing assertion pass, because those look at what the runner was HANDED. That is the
    /// worst shape a gap can have.
    ///
    /// It answers the opening turn and then one more for every line written to its stdin, which is
    /// what a real process does. A fresh `TurnSplitter` per turn, unlike the CLI runner's one across
    /// the whole stream: a canned outcome is one answer repeated, so a shared splitter would report
    /// every turn after the first as having cost nothing — true of the CLI's running total, and
    /// nonsense for a double whose whole job is to be legible.
    async fn run_prompt_with_turns(
        &self,
        mut request: RunRequest,
        session_tx: UnboundedSender<String>,
        transcript: std::sync::Arc<std::sync::Mutex<String>>,
        _context_fill: std::sync::Arc<std::sync::Mutex<Option<i64>>>,
        turn_events: Option<UnboundedSender<TurnEvent>>,
    ) -> std::io::Result<RunOutcome> {
        // Taken before the request is handed over, because the request is what carries it.
        let mut later = request.messages.take();
        let outcome = self.run_prompt(request, session_tx, transcript).await?;

        let Some(turn_events) = turn_events else {
            return Ok(outcome);
        };

        /// Records that the future holding it was dropped before it finished.
        struct Stopped {
            flag: std::sync::Arc<std::sync::Mutex<bool>>,
            armed: bool,
        }
        impl Drop for Stopped {
            fn drop(&mut self) {
                if self.armed {
                    *self.flag.lock().unwrap() = true;
                }
            }
        }
        let mut stopped = Stopped {
            flag: std::sync::Arc::clone(&self.stopped_early),
            armed: true,
        };

        let answer = |stdout: &str| {
            let mut splitter = TurnSplitter::new();
            for line in stdout.lines() {
                for event in splitter.line(line.to_owned()) {
                    let _ = turn_events.send(event);
                }
            }
        };

        answer(&outcome.stdout);
        if let Some(later) = later.as_mut() {
            // Stays alive until its stdin closes, exactly as the process does — which is what makes
            // a test of "the second turn reused the process" mean anything.
            while let Some(turn) = later.recv().await {
                self.later_turns.lock().unwrap().push(turn);
                // A process that does not answer instantly, so a test can catch a turn in flight.
                // The delay is the same knob a hung one-shot run uses.
                let waiting = *self.delay.lock().unwrap();
                if let Some(waiting) = waiting {
                    tokio::time::sleep(waiting).await;
                }
                answer(&outcome.stdout);
            }
        }
        // Reached only by ending on its own, which is what makes the flag mean "stopped".
        stopped.armed = false;
        Ok(outcome)
    }

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
        *self.last_model.lock().unwrap() = Some(request.model.clone());
        *self.last_effort.lock().unwrap() = Some(request.effort.clone());
        *self.last_fallback_model.lock().unwrap() = Some(request.fallback_model.clone());
        *self.last_add_dirs.lock().unwrap() = Some(request.add_dirs.clone());
        *self.last_max_budget_usd.lock().unwrap() = Some(request.max_budget_usd);
        *self.last_agents.lock().unwrap() = Some(request.agents.clone());
        *self.last_append_system_prompt.lock().unwrap() =
            Some(request.append_system_prompt.clone());
        *self.last_denied_tools.lock().unwrap() = Some(request.denied_tools.clone());
        *self.last_session_name.lock().unwrap() = Some(request.session_name.clone());
        *self.last_launch.lock().unwrap() = Some(Launch {
            prompt: request.prompt.clone(),
            resume_session_id: request.resume_session_id.clone(),
            session_id: request.session_id.clone(),
            fork_session: request.fork_session,
        });
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
        // The scripted agent, after the plan node's file and before anything is recorded: an
        // implement node's whole observable effect is what it left in its checkout.
        if let Some(cwd) = request.cwd.as_ref() {
            for (marker, path, contents) in self.writes.lock().unwrap().iter() {
                if request.prompt.contains(marker.as_str()) {
                    let target = cwd.join(path);
                    if let Some(parent) = target.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    std::fs::write(&target, contents).expect("the scripted agent writes its file");
                }
            }
        }
        *self.last_prompt.lock().unwrap() = Some(request.prompt.clone());
        *self.last_images.lock().unwrap() = Some(request.images.clone());
        *self.last_cwd.lock().unwrap() = request.cwd.clone();
        *self.last_permission.lock().unwrap() = Some(request.permission);
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
                compacted: false,
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

    /// A notebook write says which notebook, like every other write says which file.
    ///
    /// `detail_of` is what `ask_about` puts beside a tool name, so while `notebook_path` was off
    /// the list a `manual` rung asking about a `NotebookEdit` showed the tool and nothing else
    /// — a question about a write with the write left out, which is not a question anybody
    /// can answer. The second case pins the ORDER rather than restating the first: `description`
    /// is deliberately last, and a path must beat the sentence a model wrote about it.
    #[test]
    fn a_notebook_write_says_which_notebook() {
        assert_eq!(
            detail_of(&serde_json::json!({"notebook_path": "C:/repo/notes.ipynb"})),
            Some("C:/repo/notes.ipynb".to_owned())
        );
        assert_eq!(
            detail_of(&serde_json::json!({
                "description": "tidy the notebook up a bit",
                "notebook_path": "notes.ipynb",
            })),
            Some("notes.ipynb".to_owned())
        );
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
    fn cli_args_names_every_fallback_in_the_order_given() {
        let mut request = baseline_run_request();
        request.fallback_model = vec!["opus".to_string(), "sonnet".to_string()];

        let args = cli_args(&request, "fable");

        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--fallback-model" && pair[1] == "opus,sonnet"),
            "the fallbacks never reached the argument vector: {args:?}"
        );
    }

    #[test]
    fn cli_args_carries_every_extra_directory() {
        let mut request = baseline_run_request();
        request.add_dirs = vec![PathBuf::from("/one"), PathBuf::from("/two")];

        let args = cli_args(&request, "sonnet");

        let at = args
            .iter()
            .position(|a| a == "--add-dir")
            .expect("no --add-dir");
        assert_eq!(&args[at + 1..at + 3], ["/one", "/two"]);
    }

    /// `--add-dir` is variadic: it swallows every following argument until the next flag. On the
    /// argv path the prompt is a POSITIONAL, so a variadic flag written above it would be handed
    /// the person's message as a directory — the run would ask for tool access to their sentence
    /// and never say what it was answering.
    #[test]
    fn the_variadic_directory_flag_never_swallows_the_prompt() {
        let mut request = baseline_run_request();
        request.add_dirs = vec![PathBuf::from("/one")];

        let args = cli_args(&request, "sonnet");

        assert_eq!(args[0], "-p");
        assert_eq!(args[1], "test prompt", "the prompt moved: {args:?}");
        let at = args.iter().position(|a| a == "--add-dir").unwrap();
        assert!(at > 1, "--add-dir was written above the prompt: {args:?}");
    }

    #[test]
    fn cli_args_carries_the_ceiling_a_turn_may_spend() {
        let mut request = baseline_run_request();
        request.max_budget_usd = Some(0.5);

        let args = cli_args(&request, "sonnet");

        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--max-budget-usd" && pair[1] == "0.5"),
            "{args:?}"
        );
    }

    /// One helper, for the tests below to vary.
    fn a_helper(name: &str) -> Subagent {
        Subagent {
            name: name.to_string(),
            description: "Reviews code".to_string(),
            prompt: "You are a code reviewer".to_string(),
            tools: None,
            model: None,
            effort: None,
        }
    }

    /// The flag takes ONE argv element holding a JSON object keyed by name — not a repeatable
    /// `--agents name=…`, and not the list this daemon holds internally.
    #[test]
    fn the_helper_set_travels_as_one_object_keyed_by_name() {
        let mut request = baseline_run_request();
        request.agents = vec![a_helper("reviewer")];

        let args = cli_args(&request, "sonnet");

        let at = args
            .iter()
            .position(|arg| arg == "--agents")
            .expect("no --agents");
        let sent: serde_json::Value = serde_json::from_str(&args[at + 1]).expect("not JSON");
        assert_eq!(sent["reviewer"]["description"], "Reviews code");
        assert_eq!(sent["reviewer"]["prompt"], "You are a code reviewer");
        // The name is the KEY. Sent inside the object as well it would be an unknown field, which
        // the CLI's schema strips today — a contract that holds only because the other side is
        // forgiving, and this asserts we do not rely on that.
        assert!(
            sent["reviewer"].get("name").is_none(),
            "the name was sent twice: {}",
            args[at + 1]
        );
    }

    /// Absent keys, not null ones. `{"model": null}` is not what "inherit the conversation's model"
    /// looks like to a schema that types `model` as a string.
    #[test]
    fn a_helper_that_named_no_model_sends_no_model_key() {
        let mut request = baseline_run_request();
        request.agents = vec![a_helper("reviewer")];

        let args = cli_args(&request, "sonnet");
        let at = args.iter().position(|arg| arg == "--agents").unwrap();
        let sent: serde_json::Value = serde_json::from_str(&args[at + 1]).unwrap();

        assert!(sent["reviewer"].get("model").is_none(), "{}", args[at + 1]);
        assert!(sent["reviewer"].get("effort").is_none(), "{}", args[at + 1]);
    }

    /// Absent, not `null` and not `[]`: a helper that named no restriction inherits the parent's
    /// whole tool surface, today's behaviour, and the object sent to the CLI says nothing at all
    /// rather than saying "no restriction" in a way that could later be confused with "no tools".
    #[test]
    fn a_helper_that_named_no_tools_sends_no_tools_key() {
        let mut request = baseline_run_request();
        request.agents = vec![a_helper("reviewer")];

        let args = cli_args(&request, "sonnet");
        let at = args.iter().position(|arg| arg == "--agents").unwrap();
        let sent: serde_json::Value = serde_json::from_str(&args[at + 1]).unwrap();

        assert!(sent["reviewer"].get("tools").is_none(), "{}", args[at + 1]);
    }

    /// A helper that named a restriction sends exactly that list, so the CLI grants it those tools
    /// and nothing else.
    #[test]
    fn a_helper_may_be_restricted_to_named_tools() {
        let mut request = baseline_run_request();
        let mut helper = a_helper("reviewer");
        helper.tools = Some(vec!["Read".to_string(), "Grep".to_string()]);
        request.agents = vec![helper];

        let args = cli_args(&request, "sonnet");
        let at = args.iter().position(|arg| arg == "--agents").unwrap();
        let sent: serde_json::Value = serde_json::from_str(&args[at + 1]).unwrap();

        assert_eq!(
            sent["reviewer"]["tools"],
            serde_json::json!(["Read", "Grep"])
        );
    }

    /// A helper stored before this field existed — its JSON object has no `tools` key at all —
    /// deserialises identically to one that named no restriction, and runs the same way: inheriting
    /// the parent's whole surface, exactly as it did before this field was added.
    #[test]
    fn a_helper_stored_before_tools_existed_still_deserialises() {
        let stored = r#"{"description":"Reviews code","prompt":"You are a code reviewer"}"#;
        let agent: Subagent = serde_json::from_str(stored).unwrap();

        assert_eq!(agent.tools, None);
        assert_eq!(agent.description, "Reviews code");
        assert_eq!(agent.prompt, "You are a code reviewer");
    }

    #[test]
    fn a_helper_may_answer_on_its_own_model_and_effort() {
        let mut request = baseline_run_request();
        let mut helper = a_helper("reviewer");
        helper.model = Some("opus".to_string());
        helper.effort = Some("high".to_string());
        request.agents = vec![helper];

        let args = cli_args(&request, "sonnet");
        let at = args.iter().position(|arg| arg == "--agents").unwrap();
        let sent: serde_json::Value = serde_json::from_str(&args[at + 1]).unwrap();

        assert_eq!(sent["reviewer"]["model"], "opus");
        assert_eq!(sent["reviewer"]["effort"], "high");
        // And the conversation's own model is untouched: a helper's model is not the turn's.
        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--model" && pair[1] == "sonnet"),
            "{args:?}"
        );
    }

    /// Not `--agents {}`. The CLI answers unparseable JSON with an EMPTY agent list and no error,
    /// so from outside there is nothing to tell "a flag that parsed to nothing" from "a flag that
    /// threw" — and only one of those is a state somebody chose. Sending no flag keeps the two
    /// apart.
    #[test]
    fn a_conversation_with_no_helpers_writes_no_flag_at_all() {
        let args = cli_args(&baseline_run_request(), "sonnet");

        assert!(
            !args.iter().any(|arg| arg == "--agents"),
            "an empty helper set was sent: {args:?}"
        );
    }

    #[test]
    fn cli_args_appends_standing_instructions_rather_than_replacing_the_system_prompt() {
        let mut request = baseline_run_request();
        request.append_system_prompt = Some("Answer in Portuguese.".to_string());

        let args = cli_args(&request, "sonnet");

        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--append-system-prompt"
                    && pair[1] == "Answer in Portuguese."),
            "{args:?}"
        );
        // The REPLACING flag must never appear. It drops the CLI's tool descriptions and safety
        // framing, and a run that lost those reads as a run whose model got worse.
        assert!(
            !args.iter().any(|arg| arg == "--system-prompt"),
            "the system prompt was replaced instead of appended: {args:?}"
        );
    }

    #[test]
    fn cli_args_names_the_session_so_it_is_findable_outside_this_app() {
        let mut request = baseline_run_request();
        request.session_name = Some("o refactor do runner".to_string());

        let args = cli_args(&request, "sonnet");

        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--name" && pair[1] == "o refactor do runner"),
            "{args:?}"
        );
    }

    /// The compaction event, as a real headless stream emits it.
    ///
    /// Both lines below were copied out of `claude -p --resume` run with the window forced low, not
    /// written from the source: the point of the test is that this daemon reads what the CLI
    /// actually sends. The `compact_result` line is deliberately NOT what is read — a compaction
    /// that began is the honest answer to "was the context summarised here" whether or not it
    /// finished, and a mark that appeared and then had to be taken back would be worse than one that
    /// says where the summarising happened.
    #[test]
    fn a_compaction_is_read_off_the_stream_and_stays_read() {
        let started =
            r#"{"type":"system","subtype":"status","status":"compacting","session_id":"s"}"#;
        let finished = r#"{"type":"system","subtype":"status","status":null,"compact_result":"failed","compact_error":"too_few_groups","session_id":"s"}"#;

        assert!(
            compacted_from_line(started, false),
            "the status line says it"
        );
        assert!(
            !compacted_from_line(finished, false),
            "the result line alone is not the event"
        );
        assert!(
            compacted_from_line(finished, true),
            "a compaction already seen is not un-seen by the lines after it"
        );
        assert!(
            !compacted_from_line(r#"{"type":"assistant"}"#, false),
            "an ordinary line says nothing about compaction"
        );
        assert!(
            !compacted_from_line("not json at all", false),
            "an unparseable line is not evidence of anything"
        );
    }

    /// A compaction belongs to the turn it happened in, and to no other turn of the same process.
    ///
    /// The splitter is where this has to hold: on the multi-turn path one process answers several
    /// times, and a flag left standing would mark every later turn as the one where the
    /// conversation got shorter.
    #[test]
    fn a_compaction_marks_one_turn_and_not_the_ones_after_it() {
        let mut splitter = TurnSplitter::new();
        let result =
            r#"{"type":"result","subtype":"success","total_cost_usd":0.1,"session_id":"s"}"#;

        splitter.line(
            r#"{"type":"system","subtype":"status","status":"compacting","session_id":"s"}"#.into(),
        );
        let first = splitter.line(result.into());
        let TurnEvent::Ended(first) = &first[1] else {
            panic!("the result line ends a turn: {first:?}");
        };
        assert!(first.compacted, "the turn it happened in carries it");

        let second = splitter.line(result.into());
        let TurnEvent::Ended(second) = &second[1] else {
            panic!("the result line ends a turn: {second:?}");
        };
        assert!(
            !second.compacted,
            "the next turn did not compact, and must not inherit that it did"
        );
    }

    /// The merge that must not be two flags.
    ///
    /// `--disallowedTools` is variadic, so a second occurrence REPLACES the first rather than
    /// adding to it. Written twice, a conversation asking not to run `Bash` would have taken
    /// `ToolPolicy::McpOnly` down with it and come back holding the whole built-in tool set — a
    /// safety property undone by a preference, silently, and looking like it still held.
    #[test]
    fn a_runs_own_denials_and_its_policys_travel_as_one_flag() {
        let mut request = baseline_run_request();
        request.tool_policy = ToolPolicy::McpOnly;
        request.denied_tools = vec!["mcp__other__write".to_string()];

        let args = cli_args(&request, "sonnet");

        let flags = args
            .iter()
            .filter(|arg| arg.as_str() == "--disallowedTools")
            .count();
        assert_eq!(flags, 1, "the deny flag was written twice: {args:?}");
        let at = args.iter().position(|a| a == "--disallowedTools").unwrap();
        let denied: Vec<&str> = args[at + 1].split(',').collect();
        assert!(denied.contains(&"Bash"), "the policy's denials were lost");
        assert!(
            denied.contains(&"mcp__other__write"),
            "the run's own denial was lost"
        );
    }

    /// A conversation may narrow itself even where the policy denies nothing.
    #[test]
    fn an_unrestricted_run_still_honours_the_tools_it_was_told_not_to_use() {
        let mut request = baseline_run_request();
        request.tool_policy = ToolPolicy::Unrestricted;
        request.denied_tools = vec!["Bash".to_string(), "Edit".to_string()];

        let args = cli_args(&request, "sonnet");

        let at = args.iter().position(|a| a == "--disallowedTools").unwrap();
        assert_eq!(args[at + 1], "Bash,Edit");
    }

    /// And the wildcard stands alone. Naming individual tools beside `*` would add stderr lines
    /// about rules matching nothing, on top of a denial that already covers everything.
    #[test]
    fn the_deny_everything_policy_is_not_diluted_by_a_conversations_own_list() {
        assert_eq!(
            denied_tools(&ToolPolicy::None, &["Bash".to_string()]),
            vec!["*".to_string()]
        );
    }

    #[test]
    fn cli_args_is_silent_about_instructions_and_denials_when_none_were_chosen() {
        let args = cli_args(&baseline_run_request(), "sonnet");

        for flag in ["--append-system-prompt", "--name", "--disallowedTools"] {
            assert!(
                !args.iter().any(|arg| arg == flag),
                "{flag} was sent: {args:?}"
            );
        }
    }

    /// And says nothing when nobody asked, in all three. A flag always present would replace the
    /// CLI's own behaviour with this daemon's guess at it for every run that never expressed one.
    #[test]
    fn cli_args_is_silent_about_reach_and_ceiling_when_none_were_chosen() {
        let args = cli_args(&baseline_run_request(), "sonnet");

        for flag in ["--fallback-model", "--add-dir", "--max-budget-usd"] {
            assert!(
                !args.iter().any(|arg| arg == flag),
                "{flag} was sent: {args:?}"
            );
        }
    }

    /// The flag exists on the CLI (2.1.198, `low | medium | high | xhigh | max`) and the daemon had
    /// no way to send it. This is the half that builds it.
    #[test]
    fn cli_args_carries_the_effort_when_one_was_chosen() {
        let mut request = baseline_run_request();
        request.effort = Some("xhigh".to_string());

        let args = cli_args(&request, "sonnet");

        assert!(
            args.windows(2)
                .any(|pair| pair[0] == "--effort" && pair[1] == "xhigh"),
            "the chosen effort never reached the argument vector: {args:?}"
        );
    }

    /// And says nothing when nobody chose. An always-present flag would replace the CLI's own
    /// default with this daemon's guess at it, for every run that never asked.
    #[test]
    fn cli_args_is_silent_about_effort_when_none_was_chosen() {
        let args = cli_args(&baseline_run_request(), "sonnet");

        assert!(
            !args.iter().any(|arg| arg == "--effort"),
            "an effort was sent for a run that chose none: {args:?}"
        );
    }

    /// `--effort` must not displace `--exclude-dynamic-system-prompt-sections`, whose own comment
    /// reserves the position immediately after `--verbose` for prompt-cache prefix matching. A flag
    /// added carelessly is exactly how that invariant dies quietly.
    #[test]
    fn the_effort_flag_does_not_disturb_the_cache_prefix() {
        let mut request = baseline_run_request();
        request.effort = Some("max".to_string());

        let args = cli_args(&request, "sonnet");

        let verbose = args.iter().position(|a| a == "--verbose").unwrap();
        assert_eq!(
            args[verbose + 1],
            "--exclude-dynamic-system-prompt-sections",
            "something was inserted between --verbose and the cache flag: {args:?}"
        );
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

    /// Every rung writes exactly one flag, and the SPELLINGS are the assertion.
    ///
    /// They were measured off the installed CLI (2.1.260), whose choices are `acceptEdits`, `auto`,
    /// `bypassPermissions`, `manual`, `dontAsk` and `plan`. Two of those are traps this test exists
    /// to keep shut: `default` is what the documentation calls the unelevated rung and the command
    /// line does not accept it, so writing it — the obvious thing to do, reading the variant's name
    /// — would fail the start of every run this daemon launches; and `auto` is the CLI's OWN
    /// classifier model, a second billed opinion nobody reconciled with the classifier the daemon
    /// runs.
    ///
    /// "No flag at all" is asserted absent for the same reason it stopped being written: from
    /// v2.1.228 the default start-up mode on Pro/Max/Team plans is `auto`, so silence buys the very
    /// thing the line above refuses.
    #[test]
    fn cli_args_writes_one_measured_permission_spelling_per_rung() {
        for (permission, expected) in [
            (Permission::Default, "manual"),
            (Permission::AcceptEdits, "acceptEdits"),
            (Permission::Plan, "plan"),
            (Permission::Bypass, "bypassPermissions"),
        ] {
            let mut request = baseline_run_request();
            request.permission = permission;
            let args = cli_args(&request, "sonnet");

            assert!(
                args.windows(2)
                    .any(|pair| pair == ["--permission-mode".to_string(), expected.to_string()]),
                "{permission:?} must launch as {expected}: {args:?}"
            );
            assert_eq!(
                args.iter()
                    .filter(|arg| *arg == "--permission-mode")
                    .count(),
                1,
                "two permission modes on one command line is the CLI's choice, not ours: {args:?}"
            );
            assert!(
                !args.iter().any(|arg| arg == "default" || arg == "auto"),
                "{permission:?} wrote a spelling the CLI does not accept, or its own classifier: {args:?}"
            );
        }
    }

    /// The safety property, now unrepresentable rather than ordered.
    ///
    /// A run that must not act — a catch-up run, recovering a schedule the machine slept through,
    /// forced into planning precisely because nobody chose for it to run NOW — used to keep that
    /// restraint by winning an `else if` against `classifier_governs_tools`. The test that pinned
    /// that order said in its own words that "order is not where a safety property belongs", and it
    /// was right: there is one field now, it holds one value, and a request carrying the belief that
    /// the classifier governs cannot ALSO be carrying the flag that stands the barrier down.
    ///
    /// The belief itself survives, and this asserts that too: `classifier_governs_tools` still means
    /// something to `codex_cli_args`' refusal and to whoever reads the row — what it lost is the
    /// power to write this flag.
    #[test]
    fn a_restrained_run_cannot_also_carry_the_standing_down_flag() {
        let mut restrained = baseline_run_request();
        restrained.permission = Permission::Plan;
        restrained.classifier_governs_tools = true;
        let args = cli_args(&restrained, "sonnet");

        assert!(
            args.windows(2)
                .any(|pair| pair == ["--permission-mode".to_string(), "plan".to_string()]),
            "plan must survive: {args:?}"
        );
        assert!(
            !args.iter().any(|arg| arg == "bypassPermissions"),
            "a restrained run must never also be handed the standing-down flag: {args:?}"
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

        let line = user_message_line(text, &[]);

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
                compacted: false,
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

    /// A run offered no server is charged nothing for schemas, and that zero is an answer rather
    /// than a gap.
    ///
    /// This daemon writes `--mcp-config` or it does not; when it does not, the model is offered no
    /// tools by us and there is no schema block in its prompt to pay for.
    #[test]
    fn a_request_offered_no_server_is_charged_nothing_for_schemas() {
        let mut request = baseline_run_request();
        // `McpOnly` and not the baseline's `Unrestricted`, so that the zero below is attributable
        // to the ABSENT SERVER. An unrestricted run defers its schemas and reads 0 whatever its
        // config says, which would make the assertion pass without touching what it is about.
        request.tool_policy = ToolPolicy::McpOnly;
        assert!(request.mcp_config.is_none());
        assert_eq!(authored_prompt(&request).schema_chars, 0);
    }

    /// The same server, announced twice, charged once — because only one of the two runs was sent
    /// the schemas.
    ///
    /// **The assertion is the SPLIT, not either half.** One `RunRequest` with one `mcp_config` is
    /// read under both policies, so nothing but the regime differs between the two readings; a test
    /// that only pinned the zero would pass against code that charged nobody, and one that only
    /// pinned the price would pass against the old code that charged everybody.
    ///
    /// Measured on 2026-09-07 against CLI 2.1.263, on one prompt with one flag moved per arm. The
    /// same 48 nucleos tools cost **850** input tokens when the CLI kept `ToolSearch` and advertised
    /// them by name, and **~12,225** when every built-in was denied and it had to ship the schemas.
    /// That is the whole of why the daemon may not charge both alike: on the deferred run
    /// `advertised_schema_chars / 4` claims ~10,250 tokens for a block the model never read, and
    /// `runs::with_prompt_budget` subtracts it, so the CLI's own share is understated by as much.
    #[test]
    fn a_deferred_schema_is_not_charged_to_the_prompt_that_never_held_it() {
        let mut request = baseline_run_request();
        request.mcp_config = Some(PathBuf::from("mcp.json"));

        // The ordinary chat turn: `assistant::tool_policy_for` returns this for a turn with a cwd,
        // so the deferring regime is the common one and not the exotic one.
        request.tool_policy = ToolPolicy::Unrestricted;
        assert_eq!(
            authored_prompt(&request).schema_chars,
            0,
            "the CLI keeps `ToolSearch` here, advertises the tools by name and fetches a schema only \
             when asked — so the schema block is not in this prompt and may not be billed to it"
        );

        // One flag different. Same request, same server, same announcement.
        request.tool_policy = ToolPolicy::McpOnly;
        assert_eq!(
            authored_prompt(&request).schema_chars,
            crate::mcp_tools::NucleosTools::advertised_schema_chars(),
            "denying the built-ins denies `ToolSearch` with them, the CLI cannot defer, and the run \
             really does read every schema its server announces"
        );
    }

    /// Taking `ToolSearch` away BY NAME puts an unrestricted run back in the shipped regime, and it
    /// must be charged like one.
    ///
    /// This is the test that makes the helper's contract "ask `denied_tools`" rather than "match the
    /// policy": the policy here is `Unrestricted`, so a variant match would call this deferred and
    /// charge it nothing, while the flag `cli_args` writes says otherwise and the CLI obeys the flag.
    ///
    /// It is also where the count hypothesis dies. The regime could in principle have been chosen by
    /// how many tools were on offer rather than by which ones — so a further arm on 2026-09-07 denied
    /// every built-in EXCEPT `ToolSearch`, against the same 48-tool server: **49 tools, 5,578 input
    /// tokens, deferred**, where **48 tools with no `ToolSearch` cost 15,837 and shipped**. No count
    /// threshold makes 49 defer while 48 ships. `ToolSearch` is the variable.
    #[test]
    fn denying_tool_search_by_name_is_charged_as_a_shipped_run() {
        let mut request = baseline_run_request();
        request.mcp_config = Some(PathBuf::from("mcp.json"));
        assert!(
            matches!(request.tool_policy, ToolPolicy::Unrestricted),
            "the point of this test is a policy that would otherwise defer"
        );
        assert_eq!(
            authored_prompt(&request).schema_chars,
            0,
            "untouched, this request defers"
        );

        request.denied_tools = vec!["ToolSearch".to_string()];
        assert_eq!(
            authored_prompt(&request).schema_chars,
            crate::mcp_tools::NucleosTools::advertised_schema_chars(),
            "one name on `--disallowedTools` and the CLI has no way to fetch a schema on demand, so \
             it ships them all — the price must follow the flag, not the policy the flag sits under"
        );
    }

    fn baseline_run_request() -> RunRequest {
        RunRequest {
            prompt: "test prompt".to_string(),
            env: Vec::new(),
            cwd: None,
            permission: Permission::Default,
            resume_session_id: None,
            mcp_config: None,
            tool_policy: ToolPolicy::Unrestricted,
            progress_timeout: None,
            max_turns: None,
            session_id: Some("123e4567-e89b-42d3-a456-426614174000".to_string()),
            fork_session: false,
            include_partial_messages: false,
            images: Vec::new(),
            steerable: false,
            classifier_governs_tools: false,
            ambient_mcp: false,
            model: None,
            effort: None,
            fallback_model: Vec::new(),
            add_dirs: Vec::new(),
            max_budget_usd: None,
            agents: Vec::new(),
            append_system_prompt: None,
            denied_tools: Vec::new(),
            session_name: None,
            context_window: None,
            messages: None,
            allowed_mcp_tools: None,
        }
    }

    /// Two turns down one REAL CLI, through the argument vector this daemon actually builds.
    ///
    /// `#[ignore]` because it needs the Claude Code CLI installed, an authenticated session, and
    /// about twenty cents of somebody's money. Everything else about a conversation keeping its
    /// process is tested against `FakeCommandRunner`, which proves the wiring and proves nothing
    /// about whether the CLI serves a second turn down a stdin this code built.
    ///
    /// That exact gap has already bitten once, this month: the fake did not implement
    /// `run_prompt_with_turns` at all, so every rooted chat turn in the suite took a path that could
    /// not work — and every assertion still passed, because they all look at what the runner was
    /// HANDED rather than at what came back.
    ///
    /// What it pins: one process answering twice, one session across both, and the two turns' costs
    /// adding up to the process's own. That last one is the whole of `TurnSplitter`'s reason to
    /// exist — the CLI prints a RUNNING TOTAL on every `result`, so a second turn recorded verbatim
    /// bills the first one again, and a third bills the first two.
    #[tokio::test]
    #[ignore = "spawns the real Claude CLI and spends money; run with --include-ignored"]
    async fn a_real_cli_answers_a_second_turn_down_the_same_stdin() {
        let (messages, incoming) = tokio::sync::mpsc::unbounded_channel::<LaterTurn>();
        let (turn_events, mut events) = tokio::sync::mpsc::unbounded_channel();
        let (session_tx, mut session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        let mut request = test_run_request("Reply with the single word one.");
        request.steerable = true;
        request.messages = Some(incoming);
        request.model = Some("sonnet".to_owned());

        let runner = ClaudeCliRunner {
            model: "sonnet".to_owned(),
            plan_model: None,
            review_model: None,
        };
        let process = tokio::spawn(async move {
            runner
                .run_prompt_with_turns(
                    request,
                    session_tx,
                    std::sync::Arc::new(std::sync::Mutex::new(String::new())),
                    std::sync::Arc::new(std::sync::Mutex::new(None)),
                    Some(turn_events),
                )
                .await
        });

        let mut ended = Vec::new();
        while let Some(event) = events.recv().await {
            let TurnEvent::Ended(turn) = event else {
                continue;
            };
            ended.push(turn);
            if ended.len() == 1 {
                // The second turn, written while the process that answered the first is still
                // standing. This is the line the whole feature is about.
                messages
                    .send(LaterTurn {
                        text: "Reply with the single word two.".to_owned(),
                        images: Vec::new(),
                    })
                    .unwrap();
            } else {
                break;
            }
        }
        // Closing stdin is how a steerable run is told no more turns are coming.
        drop(messages);
        let outcome = process.await.unwrap().expect("the CLI should have run");

        assert_eq!(ended.len(), 2, "one process must have answered twice");

        let summed: f64 = ended.iter().filter_map(|turn| turn.cost_usd).sum();
        let whole = outcome
            .cost_usd
            .expect("a finished process reports what it spent");
        assert!(
            (summed - whole).abs() < 1e-6,
            "the turns must add up to the process: {summed} against {whole}"
        );

        // One session across both turns, which is what lets the conversation be resumed by it after
        // the process is gone — so losing the process costs speed and never continuity.
        let announced = session_rx
            .recv()
            .await
            .expect("the CLI announces its session");
        assert!(!announced.is_empty());
        assert!(
            session_rx.try_recv().is_err(),
            "a second session was started"
        );
    }

    /// Points `NUCLEOS_CLAUDE_BIN` at a program for as long as it lives, and puts the previous value
    /// back on drop.
    ///
    /// Under `worktree::test_env_lock`, which every test that writes the process environment takes:
    /// the variable is process-wide, and a second test pointing it elsewhere mid-run would hand this
    /// one a CLI it did not write.
    struct FakeClaudeBin {
        previous: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl FakeClaudeBin {
        fn set(program: &str) -> Self {
            let lock = crate::worktree::test_env_lock();
            let previous = std::env::var_os("NUCLEOS_CLAUDE_BIN");
            unsafe { std::env::set_var("NUCLEOS_CLAUDE_BIN", program) };
            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for FakeClaudeBin {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(value) => std::env::set_var("NUCLEOS_CLAUDE_BIN", value),
                    None => std::env::remove_var("NUCLEOS_CLAUDE_BIN"),
                }
            }
        }
    }

    /// A shell script that prints `lines` verbatim, one per line.
    fn printing(lines: &[&str]) -> String {
        let mut script = String::from("cat <<'STREAM'\n");
        for line in lines {
            script.push_str(line);
            script.push('\n');
        }
        script.push_str("STREAM\n");
        script
    }

    /// A request whose CLI is `script`, spawned through the real runner loop with `sh` as the
    /// program — so the test holds a [`FakeClaudeBin`] set to `"sh"`.
    ///
    /// It works because of where the runner puts things: `-p` first and a non-steerable run's
    /// prompt second, so a prompt that is the script's path makes `sh -p <script> <flags...>` run
    /// it, every flag after arriving as a positional argument it ignores. `-p` is `sh`'s own
    /// privileged-mode switch and harmless here. One script for every platform where a `.bat` would
    /// serve one, and `sh` is the program the gate tests already need.
    fn fake_cli(dir: &std::path::Path, script: &str) -> RunRequest {
        let path = dir.join("fake-claude.sh");
        std::fs::write(&path, script).expect("write the fake CLI");
        // Forward slashes: on Windows `sh` is MSYS, which reads `C:/...` reliably.
        test_run_request(&path.display().to_string().replace('\\', "/"))
    }

    fn claude_runner() -> ClaudeCliRunner {
        ClaudeCliRunner {
            model: "sonnet".to_owned(),
            plan_model: None,
            review_model: None,
        }
    }

    fn test_run_request(prompt: &str) -> RunRequest {
        RunRequest {
            prompt: prompt.to_string(),
            env: Vec::new(),
            cwd: None,
            permission: Permission::Default,
            resume_session_id: None,
            mcp_config: None,
            tool_policy: ToolPolicy::Unrestricted,
            progress_timeout: None,
            max_turns: None,
            session_id: None,
            fork_session: false,
            include_partial_messages: false,
            images: Vec::new(),
            steerable: false,
            classifier_governs_tools: false,
            ambient_mcp: false,
            model: None,
            effort: None,
            fallback_model: Vec::new(),
            add_dirs: Vec::new(),
            max_budget_usd: None,
            agents: Vec::new(),
            append_system_prompt: None,
            denied_tools: Vec::new(),
            session_name: None,
            context_window: None,
            messages: None,
            allowed_mcp_tools: None,
        }
    }

    /// The regression this bug was filed over. `usage` and `cost_usd` are set from the last `Ended`
    /// turn and never reset — see the `TurnEvent::Ended` arm inside `run_prompt_with_turns` — so a
    /// process that answers once and then loops into the ceiling on its NEXT turn must still report
    /// what the first one cost. Before this test existed, that property was pinned only by
    /// `a_real_cli_answers_a_second_turn_down_the_same_stdin`, which needs a paid, authenticated CLI
    /// and is `#[ignore]`d for exactly that reason — this is the same claim, proven deterministically
    /// against a real spawned process reading a scripted stream, so it runs in the gate.
    ///
    /// `steerable: false`, matching the run this bug was actually filed against: an ordinary
    /// autopilot/job run, the kind `runs.rs` spawns, not a chat turn, a council seat or a team
    /// member. What still lets a second turn exist without this request opting into steering is
    /// nothing the request controls — the CLI's own stream decides where a `result` line falls —
    /// and this fake reproduces a transcript where one already had, exactly as
    /// `create_run_inner(..., steerable: true)` lets an operator-messaged run on the Runs page do.
    #[tokio::test]
    async fn a_ceiling_death_after_one_full_turn_still_reports_that_turns_cost_and_count() {
        let dir = tempfile::tempdir().unwrap();
        let mut request = fake_cli(
            dir.path(),
            &printing(&[
                r#"{"type":"system","subtype":"init","session_id":"fake-session","tools":[]}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"turn one"}]}}"#,
                r#"{"type":"result","subtype":"success","total_cost_usd":0.05,"num_turns":1,"session_id":"fake-session","usage":{"input_tokens":11,"output_tokens":22,"cache_read_input_tokens":0,"cache_creation_input_tokens":0}}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"loop one"}]}}"#,
                r#"{"type":"assistant","message":{"content":[{"type":"text","text":"loop two"}]}}"#,
            ]),
        );
        let _bin = FakeClaudeBin::set("sh");
        // One answer for the turn that ends, two more to trip the ceiling mid-way through the turn
        // that never does: an `assistant` event with no `message.id` is an answer of its own to
        // [`TurnCounter`], so "turn one", "loop one" and "loop two" are three, and
        // `over_turn_ceiling` fires once the count reaches 3.
        request.max_turns = Some(3);
        let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        let outcome = claude_runner()
            .run_prompt(request, session_tx, discard_transcript())
            .await
            .expect("the fake CLI spawns and runs to the ceiling");

        assert_eq!(
            outcome.exit_code, TURN_CEILING_EXIT_CODE,
            "the fake transcript must trip the ceiling before its second turn ends"
        );
        assert_eq!(
            outcome.cost_usd,
            Some(0.05),
            "the first turn's cost must survive a ceiling death in the turn after it — this is \
             the NULL the owner reported, and a real number exists here to lose"
        );
        assert_eq!(
            outcome.num_turns,
            Some(1),
            "the first turn's own turn count must survive, not read back as unknown"
        );
        assert_eq!(outcome.input_tokens, Some(11));
        assert_eq!(outcome.output_tokens, Some(22));
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
                compacted: false,
            })),
            last_permission: std::sync::Mutex::new(None),
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
                compacted: false,
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

    /// The plan, which the window had as the word `TodoWrite` and nothing else.
    ///
    /// A turn that writes a plan and then works through it is the shape of most real work, and none
    /// of it reached the page: `detail_of` looks for a path or a command, a `TodoWrite` carries
    /// neither, so the call arrived as a bare name. Watching a model tick items off is half of what
    /// a person is looking at when they look at the editor.
    #[test]
    fn a_plan_is_carried_beside_the_call_that_wrote_it() {
        let stream = [message(serde_json::json!([{
            "type": "tool_use", "name": "TodoWrite",
            "input": {"todos": [
                {"content": "ler o parser", "status": "completed"},
                {"content": "arranjar as datas", "status": "in_progress"},
                {"content": "correr os testes", "status": "pending"},
            ]}
        }]))]
        .join("\n");

        let did = live_from_stream(&stream).did;

        assert_eq!(did.len(), 1);
        assert_eq!(
            did[0].todos,
            vec![
                Todo {
                    text: "ler o parser".into(),
                    status: "completed".into()
                },
                Todo {
                    text: "arranjar as datas".into(),
                    status: "in_progress".into()
                },
                Todo {
                    text: "correr os testes".into(),
                    status: "pending".into()
                },
            ]
        );
    }

    /// Every other tool carries no plan, and says so with an empty list rather than with a shape
    /// the window has to test for.
    #[test]
    fn a_call_that_is_not_a_plan_carries_no_plan() {
        let stream = message(serde_json::json!([{
            "type": "tool_use", "name": "Read", "input": {"file_path": "C:/x.rs"}
        }]));

        let did = live_from_stream(&stream).did;

        assert!(did[0].todos.is_empty());
        assert_eq!(did[0].detail.as_deref(), Some("C:/x.rs"));
    }

    /// A row written before this existed deserialises, and reads as a call that wrote no plan.
    /// `tools_used` is stored JSON: every turn already recorded is a row without the field.
    #[test]
    fn a_call_stored_before_plans_existed_still_reads() {
        let old: ToolCall =
            serde_json::from_str(r#"{"name":"Bash","detail":"cargo test"}"#).unwrap();

        assert_eq!(old.name, "Bash");
        assert!(old.todos.is_empty());
    }

    /// A subagent's call said `Task` and nothing else, which is the one call where the name alone
    /// says least: every `Task` looks like every other, and what distinguishes them is the sentence
    /// the model wrote to describe the work. It carries no path and no command, so the fixed list
    /// of keys walked straight past it.
    #[test]
    fn a_subagent_call_says_what_it_was_sent_to_do() {
        let stream = message(serde_json::json!([{
            "type": "tool_use", "name": "Task",
            "input": {"description": "rever o diff", "prompt": "olha para tudo", "subagent_type": "reviewer"}
        }]));

        let did = live_from_stream(&stream).did;

        assert_eq!(did[0].detail.as_deref(), Some("rever o diff"));
    }

    /// Ordered, not searched: a tool carrying both keeps the one that says where it acted. The
    /// description is the last resort, never the preferred answer.
    #[test]
    fn a_description_never_wins_over_the_thing_that_was_acted_on() {
        let stream = message(serde_json::json!([{
            "type": "tool_use", "name": "Edit",
            "input": {"file_path": "C:/x.rs", "description": "arranjar isto"}
        }]));

        let did = live_from_stream(&stream).did;

        assert_eq!(did[0].detail.as_deref(), Some("C:/x.rs"));
    }

    /// A turn carrying a picture is a content ARRAY, which is the API's own shape for one.
    ///
    /// Asked of the CLI before this existed, because nothing here could answer it: a `user` line
    /// whose content is an array with an `image` block in it is accepted by
    /// `--input-format stream-json`, and the model SEES it — sent a solid magenta square and asked
    /// what colour it was, it answered "Magenta", which is not a thing anybody guesses.
    ///
    /// The image comes FIRST and the words after. That is the order the API documents for a
    /// question about a picture, and the order a person types in: the screenshot, then what they
    /// want to know about it.
    #[test]
    fn a_turn_carrying_a_picture_is_written_as_a_content_array() {
        let line = user_message_line(
            "what colour is this?",
            &[Attachment {
                media_type: "image/png".into(),
                data: "aGVsbG8=".into(),
            }],
        );

        let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        let content = value
            .pointer("/message/content")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "image");
        assert_eq!(content[0]["source"]["type"], "base64");
        assert_eq!(content[0]["source"]["media_type"], "image/png");
        assert_eq!(content[0]["source"]["data"], "aGVsbG8=");
        assert_eq!(content[1]["type"], "text");
        assert_eq!(content[1]["text"], "what colour is this?");
    }

    /// A turn carrying nothing keeps the plain string it has always been.
    ///
    /// Not tidiness: the string form is the one measured working against the CLI, and every run in
    /// this daemon that is not a chat uses it. Rewriting them all as arrays to make one new case
    /// uniform would be changing what is proven to make room for what is not.
    #[test]
    fn a_turn_carrying_nothing_is_written_as_the_plain_string_it_has_always_been() {
        let line = user_message_line("arranja o parser", &[]);

        let value: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(
            value.pointer("/message/content").unwrap(),
            "arranja o parser"
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

    /* ------------------------------------------- what a tool answered -- */

    /// One `tool_result`, as the CLI sends one.
    fn answered(tool_use_id: &str, content: serde_json::Value, is_error: bool) -> String {
        serde_json::json!({
            "type": "user",
            "message": {"content": [{
                "type": "tool_result",
                "tool_use_id": tool_use_id,
                "content": content,
                "is_error": is_error
            }]}
        })
        .to_string()
    }

    /// A turn said what it REACHED FOR and never what it found.
    ///
    /// `Bash` beside `cargo test dates::`, with no way to learn from the conversation whether the
    /// tests passed — the paragraph underneath is the model's summary of exactly that, and a
    /// summary is what somebody opening a tool call has decided not to take on trust.
    #[test]
    fn a_tool_call_carries_what_the_tool_answered() {
        let stream = [
            message(serde_json::json!([{
                "type": "tool_use", "id": "toolu_1", "name": "Bash",
                "input": {"command": "cargo test dates::"}
            }])),
            answered(
                "toolu_1",
                serde_json::json!("test result: ok. 3 passed"),
                false,
            ),
        ]
        .join("\n");

        let live = live_from_stream(&stream);

        assert_eq!(live.did.len(), 1);
        assert_eq!(
            live.did[0].result.as_deref(),
            Some("test result: ok. 3 passed")
        );
        assert_eq!(live.did[0].result_chars, Some(25));
        assert!(!live.did[0].result_failed);
        // And the tool has stopped running, which is the behaviour that was already here.
        assert_eq!(live.doing, None);
    }

    /// Paired by id and never by position.
    ///
    /// The CLI runs tool calls concurrently, so answers arrive in whatever order the tools finish
    /// — "the most recent call" is wrong exactly when it matters, and the failure is quiet: two
    /// real answers, each filed under the other's question.
    #[test]
    fn two_tools_in_flight_get_their_own_answers_back() {
        let stream = [
            message(serde_json::json!([
                {"type": "tool_use", "id": "toolu_slow", "name": "Bash",
                 "input": {"command": "cargo test"}},
                {"type": "tool_use", "id": "toolu_fast", "name": "Read",
                 "input": {"file_path": "core/src/dates.rs"}}
            ])),
            // The second call answers first, which is the whole point of this test.
            answered("toolu_fast", serde_json::json!("fn is_leap_year"), false),
            answered("toolu_slow", serde_json::json!("test result: ok"), false),
        ]
        .join("\n");

        let live = live_from_stream(&stream);

        assert_eq!(live.did[0].name, "Bash");
        assert_eq!(live.did[0].result.as_deref(), Some("test result: ok"));
        assert_eq!(live.did[1].name, "Read");
        assert_eq!(live.did[1].result.as_deref(), Some("fn is_leap_year"));
    }

    /// A `Read` answers with the whole file, and the whole file does not go in a database row.
    ///
    /// The full length travels beside the cut so the window can say what it is NOT showing — a
    /// truncation presented as the whole answer is how somebody concludes a command printed
    /// nothing after the first thirty lines.
    #[test]
    fn a_long_answer_is_cut_and_says_how_long_it_really_was() {
        let whole = "x".repeat(RESULT_LIMIT + 500);
        let stream = [
            message(serde_json::json!([{
                "type": "tool_use", "id": "toolu_1", "name": "Read",
                "input": {"file_path": "big.rs"}
            }])),
            answered("toolu_1", serde_json::json!(whole), false),
        ]
        .join("\n");

        let live = live_from_stream(&stream);

        assert_eq!(
            live.did[0].result.as_ref().map(|kept| kept.chars().count()),
            Some(RESULT_LIMIT)
        );
        assert_eq!(live.did[0].result_chars, Some((RESULT_LIMIT + 500) as i64));
    }

    /// "The command failed" and "the command printed something that mentions an error" are
    /// different facts, and only the stream knows which this was.
    #[test]
    fn a_tool_that_failed_is_recorded_as_having_failed() {
        let stream = [
            message(serde_json::json!([{
                "type": "tool_use", "id": "toolu_1", "name": "Bash",
                "input": {"command": "cargo test"}
            }])),
            answered(
                "toolu_1",
                serde_json::json!("error: could not compile"),
                true,
            ),
        ]
        .join("\n");

        assert!(live_from_stream(&stream).did[0].result_failed);
    }

    /// The CLI sends `content` two ways, and both are ordinary.
    #[test]
    fn an_answer_sent_as_blocks_reads_back_as_its_text() {
        let stream = [
            message(serde_json::json!([{
                "type": "tool_use", "id": "toolu_1", "name": "Grep", "input": {"pattern": "leap"}
            }])),
            answered(
                "toolu_1",
                serde_json::json!([{"type": "text", "text": "core/src/dates.rs:12"}]),
                false,
            ),
        ]
        .join("\n");

        assert_eq!(
            live_from_stream(&stream).did[0].result.as_deref(),
            Some("core/src/dates.rs:12")
        );
    }

    /// An answer whose id names no call this stream made belongs to something that is not in this
    /// list, and is dropped rather than attached to whatever happened to be nearest.
    #[test]
    fn an_answer_to_a_call_this_stream_never_made_is_dropped() {
        let stream = [
            message(serde_json::json!([{
                "type": "tool_use", "id": "toolu_1", "name": "Read", "input": {"file_path": "a.rs"}
            }])),
            answered("toolu_somebody_else", serde_json::json!("not ours"), false),
        ]
        .join("\n");

        let live = live_from_stream(&stream);
        assert_eq!(live.did[0].result, None);
        // It still ended the wait: a tool answered, whichever one it was.
        assert_eq!(live.doing, None);
    }

    /// A turn recorded before any of this existed still reads back.
    ///
    /// `tools_used` is stored JSON, and every row already in the database is a call with none of
    /// these three fields. They default to absent, which is exactly what those turns knew.
    #[test]
    fn a_stored_call_from_before_the_answers_existed_still_parses() {
        let old: ToolCall =
            serde_json::from_str(r#"{"name":"Read","detail":"core/src/dates.rs"}"#).unwrap();

        assert_eq!(old.name, "Read");
        assert_eq!(old.result, None);
        assert_eq!(old.result_chars, None);
        assert!(!old.result_failed);
    }

    /// And a call with nothing to say about its answer says nothing on the wire.
    ///
    /// `skip_serializing_if` is what keeps the transcript the size it was: three null fields per
    /// call, over a hundred turns, on a route polled once a second.
    #[test]
    fn a_call_without_an_answer_serialises_without_the_fields() {
        let bare = ToolCall {
            name: "Read".to_string(),
            detail: None,
            todos: Vec::new(),
            result: None,
            result_chars: None,
            result_failed: false,
        };

        let json = serde_json::to_string(&bare).unwrap();

        assert!(
            !json.contains("result"),
            "the empty answer was serialised: {json}"
        );
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

    /// One line of a partial stream: a slice of THINKING as it is typed.
    ///
    /// A separate delta type from `text_delta`, and that is the whole point: the two arrive
    /// interleaved on the same stream, and a reader that took `delta.text` alone would find nothing
    /// here and show a model that sat silent through the part worth watching.
    fn pondered(text: &str) -> String {
        serde_json::json!({
            "type": "stream_event",
            "event": {"type": "content_block_delta", "index": 0,
                      "delta": {"type": "thinking_delta", "thinking": text}}
        })
        .to_string()
    }

    /// What the model thought is carried BESIDE what it said, never folded into it.
    ///
    /// The editor shows thinking; this window dropped it on the floor. `live_from_stream` kept only
    /// `text` blocks, so an answer whose reasoning WAS the work arrived as its conclusion alone —
    /// and a conclusion with nothing visible behind it is exactly the thing a reader cannot check.
    ///
    /// Beside, and not concatenated: thinking is not the reply. Joining them would put the model's
    /// working into the answer this conversation records, into the replay built from that answer,
    /// and into every place downstream that treats `text` as the thing that was said.
    #[test]
    fn what_the_model_thought_is_carried_beside_what_it_said() {
        let stream = message(serde_json::json!([
            {"type": "thinking", "thinking": "o mês vem antes do dia neste formato"},
            {"type": "text", "text": "é o parser de datas"}
        ]));

        let live = live_from_stream(&stream);

        assert_eq!(live.thought, vec!["o mês vem antes do dia neste formato"]);
        assert_eq!(live.text, "é o parser de datas");
    }

    /// Thinking streams before it completes, exactly as text does.
    ///
    /// Without this the window shows nothing at all for the longest stretch of a hard turn, and
    /// then the whole of the reasoning at once, after it has stopped being interesting.
    #[test]
    fn thinking_in_flight_reads_back_before_its_block_completes() {
        let stream = [pondered("o mês"), pondered(" vem antes")].join("\n");

        let live = live_from_stream(&stream);

        assert_eq!(live.thought, vec!["o mês vem antes"]);
        // And it did not leak into the reply: nothing has been SAID yet.
        assert_eq!(live.text, "");
    }

    /// The deltas and the completed block are the SAME thought, and a reader that took both would
    /// show it twice — the identical bug `a_completed_message_supersedes_the_deltas` guards for
    /// speech, on the channel beside it.
    #[test]
    fn a_completed_thought_supersedes_the_deltas_it_was_written_from() {
        let stream = [
            pondered("o mês"),
            pondered(" vem antes"),
            message(serde_json::json!([{"type": "thinking", "thinking": "o mês vem antes"}])),
        ]
        .join("\n");

        assert_eq!(live_from_stream(&stream).thought, vec!["o mês vem antes"]);
    }

    /// A real stream carries the SIZE of a thought and never its words.
    ///
    /// Built from a stream this machine actually produced, because the synthetic ones above are
    /// what let a feature be written that could never draw anything. Asked of the CLI directly:
    /// every `thinking` block arrives with `thinking: ""` -- 610 of them across one interactive
    /// session, not one with a word in it. Claude Code emits the signature and a running
    /// `thinking_tokens` estimate and withholds the text.
    ///
    /// So this is the honest claim a window can make about a thought, and it is the only one.
    #[test]
    fn a_thought_is_measured_because_the_cli_withholds_its_words() {
        let stream = [
            r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":50,"estimated_tokens_delta":50}"#.to_string(),
            r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":177,"estimated_tokens_delta":27}"#.to_string(),
            message(serde_json::json!([{"type": "thinking", "thinking": "", "signature": "ErwFCqUB"}])),
            message(said("17 x 23 = 391")),
        ]
        .join("
");

        let live = live_from_stream(&stream);

        assert_eq!(live.thought_tokens, Some(177));
        // And nothing is claimed to have been said in it, because nothing was.
        assert!(live.thought.is_empty());
        assert_eq!(live.text, "17 x 23 = 391");
    }

    /// A turn that did not think says so as nothing, never as zero.
    #[test]
    fn a_turn_that_did_not_think_reports_no_measurement_at_all() {
        assert_eq!(live_from_stream(&message(said("ola"))).thought_tokens, None);
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

    /// Job 26's review, as it printed it: a node that never reached the API.
    #[test]
    fn a_review_that_never_reached_the_api_failed_on_something_transient() {
        assert!(failed_on_a_transient_api_error(
            REVIEW_THAT_NEVER_REACHED_THE_API
        ));
    }

    /// No status, a timeout, rate limiting and the 5xx family are worth a second attempt; a request
    /// the API refused is not, because it will be refused again.
    #[test]
    fn only_an_api_error_a_second_attempt_can_get_past_is_transient() {
        let ended_on = |status: &str| {
            format!(
                "{{\"type\":\"system\",\"subtype\":\"init\",\"session_id\":\"s\"}}\n\
                 {{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":true,\
                 \"terminal_reason\":\"api_error\",\"api_error_status\":{status},\
                 \"result\":\"API Error\"}}"
            )
        };
        for (status, transient) in [
            ("null", true),
            ("408", true),
            ("429", true),
            ("500", true),
            ("503", true),
            ("529", true),
            ("400", false),
            ("401", false),
            ("404", false),
        ] {
            assert_eq!(
                failed_on_a_transient_api_error(&ended_on(status)),
                transient,
                "api_error_status {status}"
            );
        }
    }

    /// A turn that ended for any other reason ended on something the work did, and a stream with no
    /// result at all says nothing about why it stopped.
    #[test]
    fn a_turn_that_ended_for_any_other_reason_is_not_transient() {
        let max_turns = r#"{"type":"result","subtype":"error_max_turns","is_error":true,"terminal_reason":"max_turns","api_error_status":null,"result":""}"#;
        assert!(!failed_on_a_transient_api_error(max_turns));

        let success = r#"{"type":"result","subtype":"success","is_error":false,"result":"Nothing wrong, nothing missing.","total_cost_usd":0.12}"#;
        assert!(!failed_on_a_transient_api_error(success));

        let no_result =
            r#"{"type":"system","subtype":"api_retry","attempt":1,"error_status":null}"#;
        assert!(!failed_on_a_transient_api_error(no_result));
        assert!(!failed_on_a_transient_api_error(""));

        // The LAST result decides: an error the CLI went on past is not how the run ended.
        let recovered = format!("{REVIEW_THAT_NEVER_REACHED_THE_API}\n{success}");
        assert!(!failed_on_a_transient_api_error(&recovered));
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

    /// A turn's own lines must arrive before the word that it ended.
    ///
    /// The order is the whole contract. A consumer builds each turn's transcript out of the lines
    /// and closes it on `Ended`, so an end delivered before its own `result` line would close every
    /// turn one line short of what it actually said — and that line is the one carrying the answer.
    #[test]
    fn a_turns_lines_arrive_before_the_word_that_it_ended() {
        let mut splitter = TurnSplitter::new();
        let mut events = Vec::new();
        for line in [
            r#"{"type":"system","subtype":"init","session_id":"s"}"#,
            r#"{"type":"assistant","message":{"content":[]}}"#,
            r#"{"type":"result","subtype":"success","total_cost_usd":0.10}"#,
        ] {
            events.extend(splitter.line(line.to_string()));
        }

        assert!(matches!(events[0], TurnEvent::Line(_)));
        assert!(matches!(events[1], TurnEvent::Line(_)));
        // The result line belongs to the turn it ends, so it is a `Line` too — and only then `Ended`.
        assert!(matches!(events[2], TurnEvent::Line(_)));
        assert!(matches!(events[3], TurnEvent::Ended(_)));
        assert_eq!(events.len(), 4);
    }

    /// One process, two answers, two turns — and the second one billed for what it added rather than
    /// for everything the process had spent by then.
    #[test]
    fn two_answers_down_one_process_are_two_turns() {
        let mut splitter = TurnSplitter::new();
        let mut ended = Vec::new();
        for line in [
            r#"{"type":"result","subtype":"success","total_cost_usd":0.1046}"#,
            r#"{"type":"assistant","message":{"content":[]}}"#,
            r#"{"type":"result","subtype":"success","total_cost_usd":0.2024}"#,
        ] {
            for event in splitter.line(line.to_string()) {
                if let TurnEvent::Ended(turn) = event {
                    ended.push(turn);
                }
            }
        }

        assert_eq!(ended.len(), 2);
        assert_eq!(ended[0].cost_usd, Some(0.1046));
        assert!((ended[1].cost_usd.unwrap() - 0.0978).abs() < 1e-9);
        // And the process's own total is still available to whoever is billing the process.
        assert!((splitter.spent() - 0.2024).abs() < 1e-9);
    }

    /// A process that answers twice reports what it has spent in total, not what the last turn
    /// added. Measured on the real CLI, two turns down one stdin: `total_cost_usd` read 0.1046 and
    /// then 0.2024, so recording the second `result` verbatim bills that turn for the first one as
    /// well — and every turn after it, compounding.
    #[test]
    fn a_later_turns_cost_is_what_it_added_not_what_the_process_has_spent() {
        let first =
            r#"{"type":"result","subtype":"success","total_cost_usd":0.1046,"num_turns":1}"#;
        let second =
            r#"{"type":"result","subtype":"success","total_cost_usd":0.2024,"num_turns":1}"#;

        let (one, spent) = turn_from_result(first, 0.0).unwrap();
        assert_eq!(one.cost_usd, Some(0.1046));

        let (two, spent) = turn_from_result(second, spent).unwrap();
        assert!(
            (two.cost_usd.unwrap() - 0.0978).abs() < 1e-9,
            "expected the difference, got {:?}",
            two.cost_usd
        );
        assert!((spent - 0.2024).abs() < 1e-9);
    }

    /// The counts beside the cost are the turn's OWN — `num_turns` read 1 on both of those two
    /// results rather than 1 and 2 — so differencing them would turn a correct number into a
    /// negative one the moment a turn used fewer tokens than the turn before it.
    #[test]
    fn a_turns_token_counts_are_its_own_and_are_not_differenced() {
        let line = r#"{"type":"result","subtype":"success","total_cost_usd":0.30,"num_turns":1,"usage":{"input_tokens":40,"output_tokens":500,"cache_read_input_tokens":32194,"cache_creation_input_tokens":0}}"#;

        let (turn, _) = turn_from_result(line, 0.25).unwrap();

        assert_eq!(turn.usage.input_tokens, Some(40));
        assert_eq!(turn.usage.output_tokens, Some(500));
        assert_eq!(turn.usage.cache_read_tokens, Some(32194));
        assert_eq!(turn.usage.num_turns, Some(1));
    }

    /// Everything else on the stream is not a turn ending, and a boundary drawn on the wrong line
    /// would close a run row while the CLI was still mid-answer.
    #[test]
    fn only_a_result_line_ends_a_turn() {
        for line in [
            r#"{"type":"system","subtype":"init","session_id":"s"}"#,
            r#"{"type":"assistant","message":{"content":[]}}"#,
            "not json at all",
            "",
        ] {
            assert!(
                turn_from_result(line, 0.0).is_none(),
                "{line} must not be read as the end of a turn"
            );
        }
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
    /// Claude says `assistant` once per content block of a model message; `codex exec` says
    /// `turn.completed` once per turn. Counting both in one place is what keeps the ceiling from
    /// being a Claude-only brake — a limit that silently does not apply on one of the two paths is
    /// worse than no limit, because somebody will believe it is there.
    #[test]
    fn a_turn_is_counted_once_per_model_response_on_either_cli() {
        let claude = r#"{"type":"assistant","message":{"id":"msg_1","content":[{"type":"text","text":"hi"}]}}"#;
        let codex = r#"{"type":"turn.completed","usage":{"input_tokens":10}}"#;

        let mut turns = TurnCounter::default();
        assert!(turns.line(claude));
        assert!(turns.line(codex));
        assert_eq!(turns.count(), 2);

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
            assert!(!turns.line(quiet), "counted a turn for: {quiet}");
        }
        assert_eq!(turns.count(), 2);
    }

    /// The shape of a real answer, and the reason the counter reads ids at all: one response that
    /// says something and calls two tools is three `assistant` events with one `message.id`. Counted
    /// as three, run 900463 was stopped at 125 responses under a ceiling of 200.
    #[test]
    fn the_blocks_of_one_answer_are_one_turn_however_many_tools_it_calls() {
        let block = |id: &str, kind: &str| {
            format!(
                r#"{{"type":"assistant","message":{{"id":"{id}","content":[{{"type":"{kind}"}}]}}}}"#
            )
        };
        let mut turns = TurnCounter::default();

        assert!(turns.line(&block("msg_a", "text")));
        assert!(!turns.line(&block("msg_a", "tool_use")));
        assert!(!turns.line(&block("msg_a", "tool_use")));
        assert_eq!(turns.count(), 1, "three blocks of one answer are one turn");

        // The tool results in between are not turns, and the next answer is one.
        assert!(!turns.line(r#"{"type":"user","message":{"content":[{"type":"tool_result"}]}}"#));
        assert!(turns.line(&block("msg_b", "text")));
        // An id already counted stays counted, even with another answer's blocks in between.
        assert!(!turns.line(&block("msg_a", "text")));
        assert_eq!(turns.count(), 2);

        // With no id there is nothing to join events by, so each counts, as every event did before.
        let anonymous =
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"x"}]}}"#;
        assert!(turns.line(anonymous));
        assert!(turns.line(anonymous));
        assert_eq!(turns.count(), 4);
    }

    /// The input side of a stream, once per answer; the output side, not at all.
    #[test]
    fn a_stream_with_no_result_reports_its_input_side_once_per_answer() {
        let stdout = [
            r#"{"type":"system","subtype":"init","session_id":"s"}"#,
            r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":3,"cache_read_input_tokens":100,"cache_creation_input_tokens":10,"output_tokens":1},"content":[{"type":"text"}]}}"#,
            r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":3,"cache_read_input_tokens":100,"cache_creation_input_tokens":10,"output_tokens":1},"content":[{"type":"tool_use"}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result"}]}}"#,
            r#"{"type":"assistant","message":{"id":"m2","usage":{"input_tokens":2,"cache_read_input_tokens":200,"output_tokens":4},"content":[{"type":"text"}]}}"#,
        ]
        .join("\n");

        let usage = usage_without_a_result(&stdout);

        assert_eq!(usage.input_tokens, Some(5), "m1 once, not once per block");
        assert_eq!(usage.cache_read_tokens, Some(300));
        assert_eq!(usage.cache_creation_tokens, Some(10));
        assert_eq!(
            usage.output_tokens, None,
            "an assistant event's output count is taken mid-message; it is not the answer's total"
        );
        assert_eq!(usage.num_turns, Some(2));
    }

    /// Unknown is not zero: a stream that never answered has not reported reading nothing.
    #[test]
    fn a_stream_that_never_answered_reports_nothing_rather_than_zero() {
        let usage =
            usage_without_a_result(r#"{"type":"system","subtype":"init","session_id":"s"}"#);
        assert_eq!(usage, RunUsage::default());
    }

    /// The run this was filed over, through the real loop against a spawned process: stopped at
    /// its ceiling with no `result` ever written. A pure test of `usage_without_a_result` would
    /// pass with the function never called, and not being called is what left 900463 all NULL.
    #[tokio::test]
    async fn a_run_stopped_before_its_result_still_reports_what_its_stream_read() {
        let dir = tempfile::tempdir().unwrap();
        let mut request = fake_cli(
            dir.path(),
            &printing(&[
                r#"{"type":"system","subtype":"init","session_id":"fake","tools":[]}"#,
                r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":3,"cache_read_input_tokens":100,"cache_creation_input_tokens":10,"output_tokens":1},"content":[{"type":"text","text":"looking"}]}}"#,
                r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":3,"cache_read_input_tokens":100,"cache_creation_input_tokens":10,"output_tokens":1},"content":[{"type":"tool_use","name":"Bash","input":{}}]}}"#,
                r#"{"type":"user","message":{"content":[{"type":"tool_result"}]}}"#,
                r#"{"type":"assistant","message":{"id":"m2","usage":{"input_tokens":2,"cache_read_input_tokens":200,"cache_creation_input_tokens":0,"output_tokens":1},"content":[{"type":"text","text":"again"}]}}"#,
                r#"{"type":"assistant","message":{"id":"m3","usage":{"input_tokens":1,"cache_read_input_tokens":300,"cache_creation_input_tokens":5,"output_tokens":1},"content":[{"type":"text","text":"and again"}]}}"#,
                r#"{"type":"assistant","message":{"id":"m4","usage":{"input_tokens":1000,"cache_read_input_tokens":1000,"cache_creation_input_tokens":1000,"output_tokens":1},"content":[{"type":"text","text":"never read"}]}}"#,
            ]),
        );
        // Three responses in four events: counted by event, the ceiling would trip on m2.
        request.max_turns = Some(3);
        let _bin = FakeClaudeBin::set("sh");
        let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        let outcome = claude_runner()
            .run_prompt(request, session_tx, discard_transcript())
            .await
            .expect("the fake CLI spawns");

        assert_eq!(
            outcome.exit_code, TURN_CEILING_EXIT_CODE,
            "{}",
            outcome.stderr
        );
        assert_eq!(outcome.num_turns, Some(3), "the count that stopped it");
        assert_eq!(
            outcome.input_tokens,
            Some(6),
            "m1 once, m2, m3 — and never m4"
        );
        assert_eq!(outcome.cache_read_tokens, Some(600));
        assert_eq!(outcome.cache_creation_tokens, Some(15));
        assert_eq!(outcome.output_tokens, None);
        assert_eq!(
            outcome.cost_usd, None,
            "the budget's own time estimate covers this"
        );
    }

    /// The other side of the same line: once a `result` has arrived, its figures are the run's,
    /// and the stream's partial ones never overwrite them.
    #[tokio::test]
    async fn a_run_that_reached_its_result_reports_the_results_figures() {
        let dir = tempfile::tempdir().unwrap();
        let request = fake_cli(
            dir.path(),
            &printing(&[
                r#"{"type":"system","subtype":"init","session_id":"fake","tools":[]}"#,
                r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":3,"cache_read_input_tokens":100,"cache_creation_input_tokens":10,"output_tokens":1},"content":[{"type":"text","text":"done"}]}}"#,
                r#"{"type":"result","subtype":"success","result":"done","total_cost_usd":0.02,"num_turns":1,"session_id":"fake","usage":{"input_tokens":7,"output_tokens":50,"cache_read_input_tokens":900,"cache_creation_input_tokens":40}}"#,
            ]),
        );
        let _bin = FakeClaudeBin::set("sh");
        let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        let outcome = claude_runner()
            .run_prompt(request, session_tx, discard_transcript())
            .await
            .expect("the fake CLI spawns");

        assert_eq!(outcome.exit_code, 0, "{}", outcome.stderr);
        assert_eq!(outcome.input_tokens, Some(7));
        assert_eq!(outcome.output_tokens, Some(50));
        assert_eq!(outcome.cache_read_tokens, Some(900));
        assert_eq!(outcome.cache_creation_tokens, Some(40));
        assert_eq!(outcome.num_turns, Some(1));
        assert_eq!(outcome.cost_usd, Some(0.02));
    }

    /// Who keeps background tasks: only a run whose process outlives its turn.
    #[test]
    fn a_run_nothing_can_wake_is_given_no_background_tasks() {
        let taken = Some(("CLAUDE_CODE_DISABLE_BACKGROUND_TASKS", "1"));

        assert_eq!(background_env(&test_run_request("p")), taken);

        let mut steerable_alone = test_run_request("p");
        steerable_alone.steerable = true;
        assert_eq!(
            background_env(&steerable_alone),
            taken,
            "with no channel, stdin closes after the opening turn and nothing can wake it either"
        );

        let (_later, turns) = tokio::sync::mpsc::unbounded_channel::<LaterTurn>();
        let mut conversation = test_run_request("p");
        conversation.steerable = true;
        conversation.messages = Some(turns);
        assert_eq!(background_env(&conversation), None);
    }

    /// The signature, as run 900473's own stream wrote it.
    #[test]
    fn a_task_killed_after_the_last_answer_is_named_as_orphaned() {
        let stdout = [
            r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"I'll wait for the background gate build to finish"}]}}"#,
            r#"{"type":"result","subtype":"success","stop_reason":"end_turn","session_id":"s"}"#,
            r#"{"type":"system","subtype":"background_tasks_changed","tasks":[],"session_id":"s"}"#,
            r#"{"type":"system","subtype":"task_updated","task_id":"b84qqcytz","patch":{"status":"killed","end_time":1789005221034},"session_id":"s"}"#,
            r#"{"type":"system","subtype":"task_notification","task_id":"b84qqcytz","tool_use_id":"toolu_1","status":"stopped","session_id":"s"}"#,
        ]
        .join("\n");

        assert_eq!(
            orphaned_background_tasks(&stdout),
            vec!["b84qqcytz".to_string()],
            "one task, named once although two events report it"
        );
    }

    /// Stopped by the model mid-turn is a decision; finishing after the answer is not dying.
    #[test]
    fn a_task_stopped_mid_turn_or_finished_after_it_is_not_orphaned() {
        let stdout = [
            r#"{"type":"system","subtype":"task_updated","task_id":"early","patch":{"status":"killed"}}"#,
            r#"{"type":"result","subtype":"success"}"#,
            r#"{"type":"system","subtype":"task_notification","task_id":"late","status":"completed"}"#,
        ]
        .join("\n");

        assert!(orphaned_background_tasks(&stdout).is_empty());
    }

    /// Both lines of defence, through the real loop: the variable reaches the spawned process, and
    /// a clean exit that left a task behind is recorded as the failure it is.
    #[tokio::test]
    async fn a_headless_run_that_leaves_a_task_running_is_not_a_success() {
        let dir = tempfile::tempdir().unwrap();
        let script = printing(&[
            r#"{"type":"system","subtype":"init","session_id":"fake","tools":[]}"#,
        ]) + r#"printf '{"type":"assistant","message":{"id":"m1","content":[{"type":"text","text":"background-off=%s"}]}}\n' "$CLAUDE_CODE_DISABLE_BACKGROUND_TASKS""#
            + "\n"
            + &printing(&[
                r#"{"type":"result","subtype":"success","result":"waiting","stop_reason":"end_turn","session_id":"fake"}"#,
                r#"{"type":"system","subtype":"task_updated","task_id":"b84qqcytz","patch":{"status":"killed"},"session_id":"fake"}"#,
                r#"{"type":"system","subtype":"task_notification","task_id":"b84qqcytz","status":"stopped","session_id":"fake"}"#,
            ]);
        let request = fake_cli(dir.path(), &script);
        let _bin = FakeClaudeBin::set("sh");
        let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();

        let outcome = claude_runner()
            .run_prompt(request, session_tx, discard_transcript())
            .await
            .expect("the fake CLI spawns");

        assert!(
            outcome.stdout.contains(r#""text":"background-off=1""#),
            "the variable must reach the process it is meant for: {}",
            outcome.stdout
        );
        assert_eq!(outcome.exit_code, -1, "{}", outcome.stderr);
        assert!(
            outcome.stderr.contains("b84qqcytz"),
            "the stderr names the task: {}",
            outcome.stderr
        );
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

    /// Thinking tokens are not context, and a run with no usage line knows nothing.
    ///
    /// This read `thinking_tokens` as a fallback, and the two numbers are not the same kind of
    /// thing: one is what the model spent reasoning, the other is how full its window is. Measured
    /// against a real stream they are not even close — 177 tokens of thinking on a turn carrying
    /// 48,733 of context, out by two hundred and fifty times.
    ///
    /// It matters because of what reads this column. `get_session` refuses to resume past
    /// `CONTEXT_ROTATION_TOKENS`, and the window draws "x of 140k" under every turn: a run that
    /// recorded 177 was a run claiming to be nearly empty while it was a third full. Unknown is the
    /// honest answer, and the one the ceiling already handles — a NULL fill has never tripped it,
    /// and neither did the wrong number.
    #[test]
    fn thinking_tokens_are_not_read_as_context_because_they_are_not_context() {
        let thinking = r#"{"type":"system","subtype":"thinking_tokens","estimated_tokens":125}"#;

        assert_eq!(crate::runner::context_fill_from_line(thinking, None), None);
        // And it does not overwrite a real reading that arrived before it, either.
        assert_eq!(
            crate::runner::context_fill_from_line(thinking, Some(48_733)),
            Some(48_733)
        );
    }

    /// The whole point of a second column: the last turn and the worst moment are different facts.
    #[test]
    fn the_peak_keeps_the_fullest_line_and_not_the_last() {
        let subiu = r#"{"type":"assistant","message":{"usage":{"input_tokens":1000,"cache_read_input_tokens":189000}}}"#;
        let desceu = r#"{"type":"assistant","message":{"usage":{"input_tokens":500,"cache_read_input_tokens":39500}}}"#;

        let mut pico = None;
        pico = context_peak_from_line(subiu, pico);
        pico = context_peak_from_line(desceu, pico);

        // The last turn says 40k. The worst moment said 190k, and that is what pressure reads.
        assert_eq!(pico, Some(190_000));
    }

    #[test]
    fn a_line_that_says_nothing_about_usage_leaves_the_peak_alone() {
        assert_eq!(
            context_peak_from_line(r#"{"type":"system"}"#, Some(120_000)),
            Some(120_000)
        );
        assert_eq!(
            context_peak_from_line("not json", Some(120_000)),
            Some(120_000)
        );
    }

    #[test]
    fn the_peak_of_a_stream_that_never_spoke_is_nothing() {
        assert_eq!(context_peak_from_line("not json", None), None);
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

    /// The two tests above assert hand-picked subsets — the ones that already cost a dead run — and
    /// that is documentation worth keeping, but it left the other ~31 names in `BUILTIN_TOOLS` with
    /// no assertion at all: a name could fall out of the list on an edit and nothing here would
    /// notice. This iterates the whole const instead, so the list and the flag it produces can never
    /// drift apart silently again.
    #[test]
    fn every_built_in_name_reaches_the_deny_flag() {
        let args = args_for(ToolPolicy::McpOnly, None);
        let denied = args
            .windows(2)
            .find(|w| w[0] == "--disallowedTools")
            .map(|w| w[1].clone())
            .expect("McpOnly must deny built-ins");
        let denied: std::collections::HashSet<&str> = denied.split(',').collect();
        for tool in BUILTIN_TOOLS {
            assert!(
                denied.contains(tool),
                "{tool} is in BUILTIN_TOOLS but missing from --disallowedTools: {denied:?}"
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

        let args = codex_cli_args(&request, "gpt-5.6-terra", &CodexStaged::default())
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
        let directoryless_args =
            codex_cli_args(&directoryless, "gpt-5.6-terra", &CodexStaged::default())
                .expect("a request without a directory is still honourable");
        assert!(
            !directoryless_args.iter().any(|arg| arg == "-C"),
            "an absent cwd must not invent a directory: {directoryless_args:?}"
        );
    }

    #[test]
    fn codex_cli_args_ask_for_the_json_event_stream() {
        let fresh = codex_cli_args(
            &baseline_run_request(),
            "gpt-5.6-terra",
            &CodexStaged::default(),
        )
        .unwrap();
        assert!(fresh.iter().any(|arg| arg == "--json"), "{fresh:?}");

        let mut resumed = baseline_run_request();
        resumed.resume_session_id = Some("sess-1".to_string());
        let resumed = codex_cli_args(&resumed, "gpt-5.6-terra", &CodexStaged::default()).unwrap();
        assert!(resumed.iter().any(|arg| arg == "--json"), "{resumed:?}");
    }

    #[test]
    fn codex_cli_args_resume_a_session_through_the_resume_subcommand() {
        let mut request = baseline_run_request();
        request.resume_session_id = Some("sess-1".to_string());
        request.cwd = Some(PathBuf::from("C:/work/repo"));
        request.add_dirs = vec![PathBuf::from("C:/work/other")];

        let args = codex_cli_args(&request, "gpt-5.6-terra", &CodexStaged::default()).unwrap();
        assert_eq!(&args[..2], ["exec", "resume"], "{args:?}");
        assert!(!args.iter().any(|arg| arg == "-C"), "{args:?}");
        assert!(!args.iter().any(|arg| arg == "--add-dir"), "{args:?}");
        assert_eq!(
            &args[args.len() - 2..],
            ["sess-1", "test prompt"],
            "{args:?}"
        );
    }

    #[test]
    fn codex_cli_args_carry_effort_as_a_reasoning_override() {
        let mut request = baseline_run_request();
        request.effort = Some("high".to_string());
        let args = codex_cli_args(&request, "gpt-5.6-terra", &CodexStaged::default()).unwrap();
        assert!(
            args.windows(2)
                .any(|pair| { pair[0] == "-c" && pair[1] == r#"model_reasoning_effort="high""# }),
            "{args:?}"
        );

        let without_effort = codex_cli_args(
            &baseline_run_request(),
            "gpt-5.6-terra",
            &CodexStaged::default(),
        )
        .unwrap();
        assert!(
            !without_effort
                .iter()
                .any(|arg| arg.contains("model_reasoning_effort")),
            "{without_effort:?}"
        );
    }

    #[test]
    fn codex_cli_args_carry_extra_dirs_on_both_launch_shapes() {
        let directories = vec![PathBuf::from("C:/work/a"), PathBuf::from("C:/work/b")];
        let mut fresh = baseline_run_request();
        fresh.add_dirs = directories.clone();
        let fresh = codex_cli_args(&fresh, "gpt-5.6-terra", &CodexStaged::default()).unwrap();
        for directory in ["C:/work/a", "C:/work/b"] {
            assert!(
                fresh
                    .windows(2)
                    .any(|pair| pair[0] == "--add-dir" && pair[1] == directory),
                "{fresh:?}"
            );
        }

        let mut resumed = baseline_run_request();
        resumed.resume_session_id = Some("sess-1".to_string());
        resumed.add_dirs = directories;
        let resumed = codex_cli_args(&resumed, "gpt-5.6-terra", &CodexStaged::default()).unwrap();
        let roots = serde_json::to_string(&["C:/work/a", "C:/work/b"]).unwrap();
        assert!(
            resumed.windows(2).any(|pair| {
                pair[0] == "-c"
                    && pair[1] == format!("sandbox_workspace_write.writable_roots={roots}")
            }),
            "{resumed:?}"
        );
        assert!(!resumed.iter().any(|arg| arg == "--add-dir"), "{resumed:?}");
    }

    /// An explicit override beats the user's `~/.codex/config.toml`, which exec's own default does
    /// not; this is `-c` because `codex exec resume` has no `-s`.
    #[test]
    fn codex_cli_args_pin_the_sandbox_on_both_launch_shapes() {
        let fresh_request = baseline_run_request();
        let mut resumed_request = baseline_run_request();
        resumed_request.resume_session_id =
            Some("123e4567-e89b-42d3-a456-426614174001".to_string());
        let pinned = CodexStaged {
            sandbox_mode: Some("read-only"),
            ..CodexStaged::default()
        };

        let fresh = codex_cli_args(&fresh_request, "gpt-5.6-terra", &pinned).unwrap();
        let resumed = codex_cli_args(&resumed_request, "gpt-5.6-terra", &pinned).unwrap();
        let sandbox = r#"sandbox_mode="read-only""#;

        for args in [&fresh, &resumed] {
            assert_eq!(
                args.windows(2)
                    .filter(|pair| pair[0] == "-c" && pair[1] == sandbox)
                    .count(),
                1,
                "{args:?}"
            );
            let sandbox_index = args
                .windows(2)
                .position(|pair| pair[0] == "-c" && pair[1] == sandbox)
                .unwrap();
            assert!(sandbox_index + 1 < args.len() - 1, "{args:?}");
        }
        let resumed_sandbox_index = resumed
            .windows(2)
            .position(|pair| pair[0] == "-c" && pair[1] == sandbox)
            .unwrap();
        let session_index = resumed
            .iter()
            .position(|arg| arg == "123e4567-e89b-42d3-a456-426614174001")
            .unwrap();
        assert!(resumed_sandbox_index + 1 < session_index, "{resumed:?}");

        for request in [&fresh_request, &resumed_request] {
            let args = codex_cli_args(request, "gpt-5.6-terra", &CodexStaged::default()).unwrap();
            assert!(
                !args.iter().any(|arg| arg.starts_with("sandbox_mode=")),
                "{args:?}"
            );
        }
    }

    /// The owner decided a Codex chat turn runs read-only; the run runner in `main.rs` keeps `None`
    /// so a run keeps the user's Codex configuration.
    #[test]
    fn codex_chat_runner_pins_the_read_only_sandbox() {
        let runner = CodexCliRunner::for_chat("gpt-5.5".to_string());

        assert_eq!(runner.model, "gpt-5.5");
        assert_eq!(runner.sandbox_mode, Some("read-only"));
    }

    #[test]
    fn codex_cli_args_attach_images_without_swallowing_the_prompt() {
        let mut request = baseline_run_request();
        request.images = vec![
            Attachment {
                media_type: "image/png".to_string(),
                data: "aGVsbG8=".to_string(),
            },
            Attachment {
                media_type: "image/jpeg".to_string(),
                data: "d29ybGQ=".to_string(),
            },
        ];
        let staged = CodexStaged {
            mcp_overrides: Vec::new(),
            images: vec![
                PathBuf::from("C:/stage/one.png"),
                PathBuf::from("C:/stage/two.jpg"),
            ],
            sandbox_mode: None,
        };
        let args = codex_cli_args(&request, "gpt-5.6-terra", &staged).unwrap();
        for image in ["C:/stage/one.png", "C:/stage/two.jpg"] {
            assert!(
                args.windows(2)
                    .any(|pair| pair[0] == "-i" && pair[1] == image),
                "{args:?}"
            );
            assert!(
                args.windows(3).any(|triple| triple[0] == "-i"
                    && triple[1] == image
                    && triple[2].starts_with('-')),
                "{args:?}"
            );
        }
        assert_eq!(
            args.last().map(String::as_str),
            Some("test prompt"),
            "{args:?}"
        );

        let too_few = CodexStaged {
            mcp_overrides: Vec::new(),
            images: vec![PathBuf::from("C:/stage/one.png")],
            sandbox_mode: None,
        };
        let error = codex_cli_args(&request, "gpt-5.6-terra", &too_few).unwrap_err();
        assert!(error.contains("images"), "{error}");
    }

    #[test]
    fn codex_mcp_overrides_translate_the_daemons_mcp_config() {
        let config = serde_json::json!({"mcpServers":{"nucleos":{"type":"stdio","command":"C:/x/nucleos-core.exe","args":["--mcp-tools","a"]}}});
        let env_names = vec![
            "NUCLEOS_DAEMON_TOKEN".to_string(),
            "NUCLEOS_DAEMON_URL".to_string(),
        ];
        let overrides = codex_mcp_overrides(&config, &env_names).unwrap();
        assert_eq!(
            overrides,
            vec![
                r#"mcp_servers.nucleos.command="C:/x/nucleos-core.exe""#.to_string(),
                r#"mcp_servers.nucleos.args=["--mcp-tools","a"]"#.to_string(),
                r#"mcp_servers.nucleos.env_vars=["NUCLEOS_DAEMON_TOKEN","NUCLEOS_DAEMON_URL"]"#
                    .to_string(),
                r#"mcp_servers.nucleos.default_tools_approval_mode="approve""#.to_string(),
            ]
        );

        let mut request = baseline_run_request();
        request.env = vec![(
            "NUCLEOS_DAEMON_TOKEN".to_string(),
            "secret-token-value".to_string(),
        )];
        let args = codex_cli_args(
            &request,
            "gpt-5.6-terra",
            &CodexStaged {
                mcp_overrides: overrides.clone(),
                images: Vec::new(),
                sandbox_mode: None,
            },
        )
        .unwrap();
        for override_value in overrides {
            assert!(
                args.windows(2)
                    .any(|pair| pair[0] == "-c" && pair[1] == override_value),
                "{args:?}"
            );
        }
        assert!(
            !args.iter().any(|arg| arg.contains("secret-token-value")),
            "{args:?}"
        );
    }

    /// `codex exec` declines an unconfirmed MCP call; Claude pre-approves these same tools.
    #[test]
    fn codex_mcp_overrides_pre_approve_the_daemons_own_tools() {
        let config = serde_json::json!({"mcpServers":{"nucleos":{"type":"stdio","command":"C:/x/nucleos-core.exe","args":["--mcp-tools"]}}});
        let overrides = codex_mcp_overrides(&config, &[]).unwrap();
        assert_eq!(
            overrides,
            vec![
                r#"mcp_servers.nucleos.command="C:/x/nucleos-core.exe""#.to_string(),
                r#"mcp_servers.nucleos.args=["--mcp-tools"]"#.to_string(),
                r#"mcp_servers.nucleos.default_tools_approval_mode="approve""#.to_string(),
            ]
        );
    }

    #[test]
    fn codex_mcp_overrides_refuse_a_config_they_cannot_express() {
        let cases = [
            serde_json::json!({}),
            serde_json::json!({"mcpServers": []}),
            serde_json::json!({"mcpServers":{"nucleos":{"type":"http","command":"C:/x"}}}),
            serde_json::json!({"mcpServers":{"not nucleos":{"type":"stdio","command":"C:/x"}}}),
            serde_json::json!({"mcpServers":{"nucleos":{"type":"stdio","command":3}}}),
        ];
        for config in cases {
            let error = codex_mcp_overrides(&config, &[]).unwrap_err();
            assert!(error.contains("mcp_config"), "{error}");
        }
    }

    #[tokio::test]
    async fn the_codex_runner_refuses_an_mcp_config_it_cannot_read() {
        let runner = CodexCliRunner {
            model: "gpt-5.6-terra".to_string(),
            sandbox_mode: None,
        };
        let mut request = baseline_run_request();
        request.mcp_config = Some(PathBuf::from("C:/does-not-exist/mcp.json"));
        let (session_tx, _session_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let error = runner
            .run_prompt(
                request,
                session_tx,
                std::sync::Arc::new(std::sync::Mutex::new(String::new())),
            )
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("mcp_config"), "{error}");
        assert!(
            !error.contains("cannot honour mcp_config: it has no flag"),
            "{error}"
        );
    }

    #[test]
    fn codex_thread_id_is_read_from_the_thread_started_event() {
        assert_eq!(
            codex_thread_id(r#"{"type":"thread.started","thread_id":"t-123"}"#),
            Some("t-123".to_string())
        );
        assert_eq!(codex_thread_id(r#"{"type":"turn.completed"}"#), None);
        assert_eq!(codex_thread_id("not json"), None);
    }

    #[test]
    fn extract_reply_reads_a_codex_agent_message() {
        let codex = r#"{"type":"item.completed","item":{"type":"agent_message","text":"draft"}}
{"type":"item.completed","item":{"type":"agent_message","text":"final"}}"#;
        assert_eq!(extract_reply(codex), Some("final".to_string()));
        assert_eq!(
            extract_reply(r#"{"type":"result","result":"olá"}"#),
            Some("olá".to_string())
        );
    }

    #[test]
    fn codex_cli_args_accept_a_one_turn_steerable_request() {
        let mut one_turn = baseline_run_request();
        one_turn.steerable = true;
        assert!(codex_cli_args(&one_turn, "gpt-5.6-terra", &CodexStaged::default()).is_ok());

        let mut live = baseline_run_request();
        live.steerable = true;
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel::<LaterTurn>();
        live.messages = Some(rx);
        let error = codex_cli_args(&live, "gpt-5.6-terra", &CodexStaged::default()).unwrap_err();
        assert!(error.contains("steerable"), "{error}");
    }

    #[test]
    fn codex_images_are_staged_as_files_the_cli_can_read() {
        let temp = tempfile::TempDir::new().unwrap();
        let images = vec![
            Attachment {
                media_type: "image/png".to_string(),
                data: "cG5n".to_string(),
            },
            Attachment {
                media_type: "image/jpeg".to_string(),
                data: "anBlZw==".to_string(),
            },
        ];
        let staged = stage_codex_images(temp.path(), "stem", &images).unwrap();
        assert_eq!(staged.len(), 2, "{staged:?}");
        assert!(staged[0].ends_with("stem-0.png"), "{staged:?}");
        assert!(staged[1].ends_with("stem-1.jpg"), "{staged:?}");
        assert_eq!(std::fs::read(&staged[0]).unwrap(), b"png");
        assert_eq!(std::fs::read(&staged[1]).unwrap(), b"jpeg");

        let pdf = [Attachment {
            media_type: "application/pdf".to_string(),
            data: "cGRm".to_string(),
        }];
        let error = stage_codex_images(temp.path(), "stem", &pdf).unwrap_err();
        assert!(error.to_string().contains("images"), "{error}");
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
            codex_cli_args(&honourable, "gpt-5.6-terra", &CodexStaged::default()).is_ok(),
            "the control case must build, or a refusal proves nothing"
        );

        let mut forked = baseline_run_request();
        forked.fork_session = true;
        let forked_refusal = codex_cli_args(&forked, "gpt-5.6-terra", &CodexStaged::default())
            .expect_err("a forked session cannot be honoured here");
        assert!(
            forked_refusal.contains("fork_session"),
            "the refusal must name what it could not honour: {forked_refusal}"
        );

        let mut steerable = baseline_run_request();
        steerable.steerable = true;
        let (_tx, rx) = tokio::sync::mpsc::unbounded_channel::<LaterTurn>();
        steerable.messages = Some(rx);
        let steerable_refusal =
            codex_cli_args(&steerable, "gpt-5.6-terra", &CodexStaged::default())
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
            sandbox_mode: None,
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
    /// `permission` is why this is a refusal rather than a warning. `Plan` is how a run is made
    /// unable to act — a catch-up run, recovering a schedule the machine slept through, is forced
    /// into it precisely because nobody chose for it to run now — so a runner that ignores it
    /// converts a deliberately restrained run into an unrestrained one, in the one case where the
    /// operator is not watching. `Bypass` is refused for the opposite reason and it is the sharper
    /// of the two: it stands the CLI's permission barrier down on the strength of a `PreToolUse`
    /// hook this launch surface has never heard of.
    ///
    /// `mcp_config` and `resume_session_id` are deliberately NOT here, because both are honoured now:
    /// the MCP config becomes `-c mcp_servers.<name>...` overrides (see `codex_mcp_overrides` and its
    /// tests), and a resumed run continues its own thread through `codex exec resume`.
    ///
    /// Asserted against `run_prompt` rather than `codex_cli_args`, because that builder's purity
    /// contract is frozen. Cheap for the same reason it is safe: every case returns before the
    /// spawn, so no `codex` binary has to exist for this to run.
    #[tokio::test]
    async fn the_codex_runner_refuses_flags_it_cannot_honour() {
        let runner = CodexCliRunner {
            model: "gpt-5.6-terra".to_string(),
            sandbox_mode: None,
        };

        let mut restrained = baseline_run_request();
        restrained.permission = Permission::Plan;
        // The other direction, and the one that fails open rather than closed: honouring it would
        // mean running unbarriered where the daemon believes a classifier took over.
        let mut unbarriered = baseline_run_request();
        unbarriered.permission = Permission::Bypass;
        // The per-conversation controls of 0110–0113. Each of these is drawn back at the person in
        // the window as a setting their conversation has, so a launch that dropped one would leave
        // the row claiming something the run never had.
        let mut degrading = baseline_run_request();
        degrading.fallback_model = vec!["opus".to_string()];
        let mut capped = baseline_run_request();
        capped.max_budget_usd = Some(0.5);
        let mut helped = baseline_run_request();
        helped.agents = vec![a_helper("reviewer")];
        let mut instructed = baseline_run_request();
        instructed.append_system_prompt = Some("Answer in Portuguese.".to_string());
        // The sharpest of them: it passes the `ToolPolicy` guard, because a conversation can be
        // unrestricted and still have barred a tool for itself.
        let mut barred = baseline_run_request();
        barred.denied_tools = vec!["Bash".to_string()];

        for (field, request) in [
            ("Permission::Plan", restrained),
            ("Permission::Bypass", unbarriered),
            ("fallback_model", degrading),
            ("max_budget_usd", capped),
            ("agents", helped),
            ("append_system_prompt", instructed),
            ("denied_tools", barred),
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
            codex_cli_args(&honourable, "gpt-5.6-terra", &CodexStaged::default()).is_ok(),
            "a request asking for none of the above must still build a launch"
        );

        // Resume is honoured, through `codex exec resume`, and must still build a launch.
        let mut resumed = baseline_run_request();
        resumed.resume_session_id = Some("123e4567-e89b-42d3-a456-426614174001".to_string());
        assert!(
            codex_cli_args(&resumed, "gpt-5.6-terra", &CodexStaged::default()).is_ok(),
            "resume is honoured through `exec resume`"
        );

        // And the other one, for the same reason. A display name reaches the Claude CLI's `--resume`
        // picker and nothing else; no record claims it was applied, so losing it costs a nameless
        // session, which is what every run on this path has always had.
        let mut named = baseline_run_request();
        named.session_name = Some("o refactor do runner".to_string());
        let (session_tx, _rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let transcript = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let outcome = runner
            .run_prompt(named, session_tx, transcript)
            .await
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(
            !outcome.contains("session_name"),
            "a cosmetic name is dropped on this path, not refused: {outcome}"
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
