//! What a run is asked to do and what it hands back: exit codes, the turn ceiling, tool policy, permissions, `RunRequest` and `RunOutcome`.

use super::*;

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
/// A project may move it for its own runs with `max_turns` in its autopilot rules, never remove it.
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
pub struct TurnCounter {
    pub(super) count: i64,
    pub(super) seen: std::collections::HashSet<String>,
}

impl TurnCounter {
    /// Folds one line in, and answers whether it began a response this counter had not seen yet.
    pub fn line(&mut self, line: &str) -> bool {
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
    pub fn count(&self) -> i64 {
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
pub fn over_turn_ceiling(turns: i64, ceiling: Option<i64>) -> bool {
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
    /// Whether the run reached the point a model request may be in flight (the CLI's `init`).
    ///
    /// The terminal write repairs `runs.model_requested_at` from it, so a best-effort marker write
    /// that was lost cannot leave an armed run priced at zero. `false` from every runner that
    /// cannot tell, which is harmless: an unarmed run is priced by the estimate either way.
    pub model_requested: bool,
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
    /// The job the server named by `mcp_config` is boxed to.
    ///
    /// `None` names the unboxed server. This is only `Some` together with `mcp_config`.
    pub mcp_job: Option<i64>,
    /// The team agent's node run id the server named by `mcp_config` is boxed to (`--box team
    /// --run <id>`). `None` for every run that is not a team node; only `Some` together with
    /// `mcp_config`, and never together with `mcp_job`. Read only by [`authored_prompt`], to price
    /// the Team box instead of the unboxed server.
    pub mcp_team_run: Option<i64>,
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
    /// Fired once, at the first point a model request may be in flight; `None` watches nothing.
    ///
    /// "May have been sent", never "answered": pricing reads its absence on a finished, watched
    /// run as a run that cost nothing. Only a runner whose `reports_model_request` is `true` ever
    /// fires it, so a caller arms one only after asking. A stream that ends first drops it unsent.
    pub request_tx: Option<tokio::sync::oneshot::Sender<()>>,
    /// Names of the NucleOS registry servers `mcp_config` carries beside `nucleos`, sorted.
    ///
    /// Read only to compare spawn shapes; the servers themselves travel in the config file. Empty
    /// everywhere but a chat turn.
    pub mcp_servers: Vec<String>,
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
    pub allowed_mcp_tools: Option<Vec<String>>,
    /// Keep background tasks even though nothing can wake this run once its turn ends.
    ///
    /// Set by chat turns only: a person is watching, and a task that dies with the turn is still
    /// worth seeing launched. With this set the run is not failed for leaving a task behind — see
    /// [`background_env`] and [`orphaned_background_tasks`].
    pub background_tasks: bool,
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
    /// True writes a `control_request` interrupt instead of a user turn; text and images are ignored.
    pub interrupt: bool,
    /// Some writes a `control_request` that stops that one task (a subagent or a background shell)
    /// and nothing else; text and images are ignored, and the turn goes on.
    pub stop_task: Option<String>,
}
