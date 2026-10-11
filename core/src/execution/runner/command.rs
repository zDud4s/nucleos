//! The `CommandRunner` trait every model backend implements, and the prompt helpers they share.

use super::*;

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

    /// The per-role reasoning effort, or `None` for the CLI's own default. Defaulted for the same
    /// reason as `model_for_stage`, and only the stage the CLI runner configures answers `Some`.
    fn effort_for_stage(&self, _stage: Option<&str>) -> Option<String> {
        None
    }

    /// The llm-router this runner consults before a run, or `None` when routing is off.
    ///
    /// Defaulted to `None`, and only `route_advice::RoutedRunner` answers `Some`: that wrapper is
    /// built by `main.rs` only when `router.yaml` turns routing on, so a daemon with routing off
    /// holds the very runner it held before routing existed. A capability on the trait rather than
    /// an `AppState` field, so no state literal anywhere had to learn about it.
    fn router(&self) -> Option<std::sync::Arc<crate::models::route_advice::Router>> {
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
pub fn authored_prompt(request: &RunRequest) -> crate::prompt_budget::AuthoredPrompt {
    crate::prompt_budget::AuthoredPrompt {
        schema_chars: match request.mcp_config {
            None => 0,
            // Announced but never sent: charged nothing, because nothing was read. This zero is a
            // different fact from the one above it, and `AuthoredPrompt::schema_chars` is where the
            // two are told apart for whoever reads the stored number.
            Some(_) if schemas_are_deferred(request) => 0,
            // The box the server was launched in, as `--box` names it: a team node's, a job
            // node's, or the unboxed server's.
            Some(_) => crate::mcp_tools::NucleosTools::advertised_box_schema_chars(&match (
                request.mcp_team_run,
                request.mcp_job,
            ) {
                (Some(node), _) => crate::mcp_tools::McpBox::Team(node),
                (None, Some(job)) => crate::mcp_tools::McpBox::JobNode(job),
                (None, None) => crate::mcp_tools::McpBox::All,
            }),
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
pub(super) fn schemas_are_deferred(request: &RunRequest) -> bool {
    // Deferral is a CAPABILITY, so the question is whether the run still has it: `ToolSearch` on
    // the denied list is the CLI being unable to fetch a schema on demand, which is the shipped
    // regime. Reading the same list the flag is built from is the whole point of asking here.
    !denied_tools(&request.tool_policy, &request.denied_tools)
        .iter()
        .any(|name| name == "ToolSearch")
}
