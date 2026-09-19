//! What a role needs from a model, what a model declares, and what each role does about the gap.
//!
//! Exactly one capability is verified today: the context window, read three times over in
//! `main.rs` (local triage, voice cleanup, the local assistant). Local triage and voice cleanup
//! read it through Ollama's `/api/show` probe and `runner::interpret_context_probe`; the local
//! assistant goes through `discover_local_as`, which asks `/api/show` or an OpenAI-compatible
//! `GET /models` by whichever engine `config::ModelsConfig::local_engine` resolved, the second of
//! which decides its window in `resolve_context_window` instead. One capability either way.
//!
//! This module replaced the three hand-written copies with one uniform check that produces exactly
//! the postures those three sites already implemented by hand — it changed no behaviour, it only
//! made the behaviour that already existed verifiable in code.
//!
//! It deliberately does **not** collapse the three postures into one. `local_triage_model` switches
//! off entirely when its probe fails, because the alternative is mail bodies leaving the machine.
//! `voice_cleanup_model` degrades — cleanup off, dictation continues raw — because the alternative
//! sends nothing out; it costs only the polish. `local_assistant_model` falls back to the cloud CLI,
//! because switching off would take the conversation down and the alternative already exists. A
//! global "capability missing -> error" would erase all three differences at once; the posture is
//! per role and stays per role (`triage_posture`, `voice_posture`, `local_assistant_posture`).
//!
//! The capability *vocabulary* is wider than what any role checks today — tool calling and vision
//! are here for a later packet's model picker — but the three roles this packet governs declare
//! only the context window they already enforce. Adding a requirement they do not check today would
//! be a behaviour change no configuration asked for; `nenhum_dos_tres_papeis_exige_hoje_mais_do_que_a_janela`
//! below is the guard against that happening by accident.
//!
//! Every decision function here is PURE — no network, no async runtime, testable with neither —
//! the same shape `runner::interpret_context_probe` and `runner::local_triage_decision` already
//! have and the reason they are testable at all. `main.rs` calls the pure functions; it never
//! reimplements what they decide.

use crate::runner;

/// What a role needs from a model before it may run at full capability.
///
/// `context_tokens` is compared against a model's own declared window (via
/// `runner::interpret_context_probe`, never a second copy of that reading); the other three fields
/// are declared BOOLEANS this module is the first to check at all — nothing in the crate reads
/// Ollama's `capabilities` array or OpenRouter's `supported_parameters` today.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Requirement {
    pub context_tokens: usize,
    pub tools: bool,
    pub vision: bool,
    pub structured_output: bool,
}

/// What a model declares, from whichever route discovered it.
///
/// `context` is the OUTCOME of checking this declaration's own window against the requirement that
/// triggered discovery, not a bare number — carrying `Result<(), ModelError>` through is what lets
/// `missing_capabilities` hand back the exact operator-readable reason `runner::local_triage_decision`
/// already produces for it, instead of a second, looser copy of that message built from a raw token
/// count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declared {
    pub context: Result<(), runner::ModelError>,
    pub tools: bool,
    pub vision: bool,
    pub structured_output: bool,
}

/// One capability a role required that the model did not (fully) declare.
///
/// `ContextWindow` carries the underlying `runner::ModelError` rather than a re-derived pair of
/// numbers, so a caller wanting the operator-facing reason can hand it straight to
/// `runner::local_triage_decision` and get back the exact wording that function already owns —
/// there is no second copy of that prose to keep in sync.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MissingCapability {
    ContextWindow(runner::ModelError),
    Tools,
    Vision,
    StructuredOutput,
}

/// PURE: requirement x declared -> every capability missing, in requirement order (context window,
/// then tools, then vision, then structured output) so a caller never has to guess whether only the
/// first gap was reported.
///
/// A field the source did not declare (`Declared::tools == false` because nothing said otherwise)
/// counts as ABSENT, never as present — the same fail-closed posture `interpret_context_probe`
/// already takes on every missing or unreadable field of its own response.
pub fn missing_capabilities(
    requirement: &Requirement,
    declared: &Declared,
) -> Vec<MissingCapability> {
    let mut missing = Vec::new();
    if let Err(error) = &declared.context {
        missing.push(MissingCapability::ContextWindow(error.clone()));
    }
    if requirement.tools && !declared.tools {
        missing.push(MissingCapability::Tools);
    }
    if requirement.vision && !declared.vision {
        missing.push(MissingCapability::Vision);
    }
    if requirement.structured_output && !declared.structured_output {
        missing.push(MissingCapability::StructuredOutput);
    }
    missing
}

/// What a role does once its own requirement is not fully met.
///
/// The four roles' outcomes deliberately do not share a shape beyond carrying a reason: `Unaffected`
/// is silent because nothing is missing, and the other three are worded differently on purpose —
/// collapsing them into one "degraded" case is exactly the bug §"A correction to the plan" warns
/// against reintroducing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Posture {
    /// Nothing was missing; the role runs exactly as it does when its requirement is met.
    Unaffected,
    /// The role refuses to run at all. Local triage's posture: the alternative is mail leaving the
    /// machine, so a bad probe must take triage down rather than run degraded or fall back.
    SwitchedOff(String),
    /// The role keeps running with an enhancement turned off. Voice cleanup's posture: the
    /// alternative is a raw transcript that never leaves the laptop, so a bad probe costs only the
    /// polish, never the dictation.
    Degraded(String),
    /// The role's local path is skipped and the cloud CLI answers instead. The local assistant's
    /// posture: switching off would take the conversation down, and the CLI fallback already exists.
    FellBackToCli(String),
}

/// PURE: local triage's posture — switches off completely on any gap and says why, in the exact
/// words `runner::local_triage_decision` already produces for the underlying `ModelError`. Local
/// triage is the one role for which "disabled" is spelled out today (`runner::LocalTriageDecision`)
/// and this function must not invent a second wording for the same failure.
pub fn triage_posture(missing: &[MissingCapability]) -> Posture {
    if missing.is_empty() {
        Posture::Unaffected
    } else {
        Posture::SwitchedOff(missing_reason(missing))
    }
}

/// PURE: voice cleanup's posture — a gap turns cleanup off and leaves dictation running raw. Never
/// `SwitchedOff`: the alternative to a degraded cleanup is not silence, it is a transcript that was
/// always going to be delivered anyway.
pub fn voice_posture(missing: &[MissingCapability]) -> Posture {
    if missing.is_empty() {
        Posture::Unaffected
    } else {
        Posture::Degraded(missing_reason(missing))
    }
}

/// PURE: the local assistant's posture — a gap sends the turn to the cloud CLI instead of refusing
/// it. Never `SwitchedOff`: unlike triage, this turn reads only the daemon's own state, so falling
/// back costs nothing this feature does not already accept elsewhere.
pub fn local_assistant_posture(missing: &[MissingCapability]) -> Posture {
    if missing.is_empty() {
        Posture::Unaffected
    } else {
        Posture::FellBackToCli(missing_reason(missing))
    }
}

/// The operator-facing reason shared by all three postures above.
///
/// When a context-window gap is among `missing`, this is `runner::local_triage_decision`'s own
/// wording for the underlying `ModelError` — never a second copy of it, per this module's own doc.
/// Otherwise it names the missing capabilities directly: no role checks anything but the window
/// today (`nenhum_dos_tres_papeis_exige_hoje_mais_do_que_a_janela` is the guard), so this branch
/// exists only for the wider vocabulary a later packet's model picker will exercise.
fn missing_reason(missing: &[MissingCapability]) -> String {
    let context_error = missing.iter().find_map(|capability| match capability {
        MissingCapability::ContextWindow(error) => Some(error.clone()),
        MissingCapability::Tools
        | MissingCapability::Vision
        | MissingCapability::StructuredOutput => None,
    });
    if let Some(error) = context_error {
        return match runner::local_triage_decision(Err(error)) {
            runner::LocalTriageDecision::Disabled(reason) => reason,
            runner::LocalTriageDecision::Enabled => {
                unreachable!("an Err(_) probe always decides Disabled")
            }
        };
    }

    let names: Vec<&str> = missing
        .iter()
        .map(|capability| match capability {
            MissingCapability::ContextWindow(_) => {
                unreachable!("a context-window gap is handled above")
            }
            MissingCapability::Tools => "tools",
            MissingCapability::Vision => "vision",
            MissingCapability::StructuredOutput => "structured output",
        })
        .collect();
    format!("the model does not declare: {}", names.join(", "))
}

/// PURE: why the picker must refuse this choice, or `None` when the model serves.
///
/// One sentence naming EVERY missing capability, not just the first — `missing_capabilities`
/// already returns them all in order and deliberately does not short-circuit, and a picker refusal
/// that dropped everything but the first gap would send somebody chasing a window fix while a
/// second, unrelated gap still waited for them. A context-window gap must carry BOTH numbers, what
/// the model has and what the role needs, because "too small" alone tells nobody what to act on —
/// `runner::ModelError` already carries that wording via `runner::local_triage_decision`, and this
/// reuses it rather than writing a second copy, exactly as `triage_posture` above reuses it.
///
/// No production caller until the model picker's door wires onto this — GREEN's job, not this
/// phase's; `um_modelo_que_serve_nao_da_recusa_nenhuma_no_picker` and its siblings below exercise it
/// in the meantime.
#[cfg_attr(not(test), allow(dead_code))]
pub fn picker_refusal(missing: &[MissingCapability]) -> Option<String> {
    if missing.is_empty() {
        return None;
    }

    // The context-window gap gets `runner::local_triage_decision`'s own wording (both numbers,
    // never re-derived here) as its own clause; every other gap is named alongside it in one
    // clause of its own — never dropped for having a neighbour, per this function's own doc.
    let mut clauses: Vec<String> = Vec::new();
    let mut names: Vec<&str> = Vec::new();
    for capability in missing {
        match capability {
            MissingCapability::ContextWindow(error) => {
                let reason = match runner::local_triage_decision(Err(error.clone())) {
                    runner::LocalTriageDecision::Disabled(reason) => reason,
                    runner::LocalTriageDecision::Enabled => {
                        unreachable!("an Err(_) probe always decides Disabled")
                    }
                };
                clauses.push(reason);
            }
            MissingCapability::Tools => names.push("tools"),
            MissingCapability::Vision => names.push("vision"),
            MissingCapability::StructuredOutput => names.push("structured output"),
        }
    }
    if !names.is_empty() {
        clauses.push(format!("the model does not declare: {}", names.join(", ")));
    }

    Some(clauses.join("; "))
}

/// Ollama route: `POST {base_url}/api/show`.
///
/// The window half of this reuses `runner::interpret_context_probe` — never a second copy of that
/// reading, per this module's own doc — so the only new work here is Ollama's `capabilities` array,
/// which `/api/show` already returns beside `model_info` and which nothing in this crate reads
/// before this module: short strings such as `"completion"`, `"tools"`, `"vision"`, mapped straight
/// onto this module's own vocabulary. A response this crate cannot parse at all — the same failure
/// `interpret_context_probe` already names `UnparseableResponse` — declares NOTHING: every boolean
/// field comes back `false`, never guessed `true` from silence.
///
/// Scoped to the non-test build, for the same reason `discover_openai_compatible` below is: every
/// production call site now goes through `discover_ollama_as` directly so it can pass its own
/// probe label, which leaves this four-argument wrapper with no caller but its own tests —
/// `a_descoberta_le_a_janela_de_uma_resposta_do_api_show` and its siblings below exercise it, and
/// the frozen 22 keep it from being deleted.
#[cfg_attr(not(test), allow(dead_code))]
pub async fn discover_ollama(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    required_tokens: usize,
) -> Declared {
    discover_ollama_as(client, base_url, model, required_tokens, "local model").await
}

/// The same route as `discover_ollama`, with the probe named for the caller that ran it.
///
/// Before this module existed, `main.rs` carried three hand-written copies of this probe — local
/// triage, voice cleanup, and the local assistant — and each named itself in its own
/// `could not read the {probe} probe response: {error}` message: `"local model"`, `"voice
/// cleanup"`, `"local assistant"`. Unifying the three call sites onto one function collapsed that
/// wording to whichever label the shared function hardcoded, which is a silent change on an
/// operator-facing path — the reason string lands in `AppState.local_triage_disabled` for triage
/// and in a `tracing::warn!` for the other two — even though this module's whole contract is that
/// unifying the *code* changes no *behaviour*. `probe` exists so each of the three call sites in
/// `main.rs` can keep naming itself, and `discover_ollama` above is kept only so the label a
/// caller does not pass still defaults to the one triage already used.
pub async fn discover_ollama_as(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    required_tokens: usize,
    probe: &str,
) -> Declared {
    let body = match client
        .post(format!("{base_url}/api/show"))
        .json(&serde_json::json!({ "model": model }))
        .send()
        .await
    {
        Ok(response) => match response.text().await {
            Ok(body) => body,
            Err(error) => {
                return undeclared(runner::ModelError::UnparseableResponse(format!(
                    "could not read the {probe} probe response: {error}"
                )));
            }
        },
        Err(error) => {
            return undeclared(runner::ModelError::UnparseableResponse(format!(
                "could not reach the loopback Ollama endpoint: {error}"
            )));
        }
    };

    let context = runner::interpret_context_probe(&body, required_tokens);
    let capabilities = serde_json::from_str::<serde_json::Value>(&body)
        .map(|value| string_array(&value, "capabilities"))
        .unwrap_or_default();

    Declared {
        context,
        tools: capabilities.iter().any(|capability| capability == "tools"),
        vision: capabilities.iter().any(|capability| capability == "vision"),
        // Ollama's `/api/show` has no token for this today — never guessed true from silence.
        structured_output: false,
    }
}

/// One probe for BOTH local dialects, chosen by the engine the operator actually configured.
///
/// `main.rs`'s local-assistant startup called `discover_ollama_as` against
/// `runner::OLLAMA_BASE_URL` directly, which made the dialect a property of the CALL SITE rather
/// than of the configuration. On a `local_engine: openai_compatible` install that probe posts `/api/show` to a
/// llama.cpp or LM Studio server that has no such route, the reading fails closed exactly as this
/// module's doc requires, `local_assistant_posture` answers `FellBackToCli`, and the local
/// assistant is reported disabled on a machine whose server was answering all along. The engine is
/// read from the file in exactly ONE place -- `config::ModelsConfig::local_engine` -- and this
/// function is how that one reading reaches the probe.
///
/// `role` is kept on BOTH arms even though it is inert on the OpenAiCompatible one. `discover_ollama_as`
/// interpolates it into the `could not read the {probe} probe response` wording an operator
/// actually reads, while `discover_openai_compatible` names no probe at all: its failures ride in
/// `Declared.context` and are worded once, by `missing_capabilities`. Dropping it would mean the
/// two arms take different arguments, and then every call site has to decide which dialect it is
/// talking to in order to know what to pass -- which is precisely the shape that let a hardcoded
/// engine survive at three call sites in the first place. ONE signature for every call site is what
/// stops a per-engine dialect choice re-growing at each of them.
///
/// `declared` is the same argument in the other direction: only the OpenAI-compatible route can use
/// a window the operator declared (`local_context_tokens`), because `/api/show` states Ollama's own
/// and a declared number there would be second-guessing a server about a model it is serving.
pub async fn discover_local_as(
    client: &reqwest::Client,
    engine: crate::config::LocalEngine,
    base_url: &str,
    model: &str,
    required_tokens: usize,
    declared: Option<usize>,
    role: &str,
) -> Declared {
    match engine {
        crate::config::LocalEngine::Ollama => {
            discover_ollama_as(client, base_url, model, required_tokens, role).await
        }
        crate::config::LocalEngine::OpenAiCompatible => {
            discover_openai_compatible(client, base_url, model, required_tokens, declared).await
        }
    }
}

/// A `Declared` whose every field failed closed, for a network- or transport-level failure that
/// never reaches a parseable body at all — before either route has a response to read a single
/// capability field from.
fn undeclared(context: runner::ModelError) -> Declared {
    Declared {
        context: Err(context),
        tools: false,
        vision: false,
        structured_output: false,
    }
}

/// Reads a top-level string array off a parsed JSON value, tolerating non-string entries by
/// dropping them one at a time rather than discarding the whole array — the same lesson a null
/// array element already cost elsewhere in this crate (see `map_intent.rs`'s `Option<String>`
/// fields): one malformed entry must cost its own row, never the whole reading.
fn string_array(value: &serde_json::Value, key: &str) -> Vec<String> {
    value
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Where the context window in force came from: the server, or the operator.
///
/// The two are different CLAIMS, and collapsing them to a bare `usize` loses the only thing that
/// tells a measured fact about a server apart from a number somebody wrote in a configuration file.
/// A caller that cannot tell them apart reports the second as if it were the first — the same
/// confusion this module refuses everywhere else by failing closed rather than guessing. Carrying
/// the source is also what lets `discover_openai_compatible` below say, in one log line, which number an
/// operator is actually running under.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextWindow {
    /// The catalogue itself published `context_length` for this model. Measured.
    Discovered(usize),
    /// The catalogue published nothing and the operator stated the window. Stated.
    Declared(usize),
}

/// PURE: an OpenAI-compatible `/models` body + a possibly-declared window -> the window in force.
///
/// Split out of `discover_openai_compatible` below for exactly the reason this module already splits
/// `interpret_tags` from `discover_models` and `runner::interpret_context_probe` from
/// `discover_ollama_as`: READING a body and FETCHING one are different jobs, and only the first is
/// testable with no listener anywhere near it. Nothing here touches a socket.
///
/// The order of the decision is the whole content of the function:
///
/// 1. a `context_length` the catalogue itself published -> `Discovered`. The measured number always
///    beats the stated one. Letting a declared 8192 override a probed 32768 would let configuration
///    silently truncate a server that told the truth, and a truncated prompt is reported by nobody:
///    it reads as a forgetful model rather than as a wrong number.
/// 2. no `context_length`, but `declared` is present -> `Declared`. Discovery on its own calls this
///    `MissingContextLength` and refuses, which is right when nothing is known; here the operator
///    has stated the window, so the stated number is the answer — carried as a different variant
///    because it is a different claim, not as a `Discovered` the server never made.
/// 3. neither -> `Err(MissingContextLength)`, never a vendor default. A turn ACCUMULATES — system
///    message, question, then every tool schema and every tool result again on every round — so a
///    window assumed from a guess is how a long conversation becomes a short one nobody was told
///    about. That is `local_agent::TURN_NUM_CTX`'s guard restated on a wire with no `num_ctx` field
///    to state it on.
///
/// `required_tokens` is then checked against whichever number won, because a stated window that is
/// too small is exactly as unusable as a probed one that is — `ContextTooSmall` either way. A model
/// absent from `data` keeps `ModelUnavailable` and a body that is not JSON keeps
/// `UnparseableResponse`: both fail closed, per this module's own doc, and neither is guessed past.
pub fn resolve_context_window(
    catalogue_body: &str,
    model: &str,
    declared: Option<usize>,
    required_tokens: usize,
) -> Result<ContextWindow, runner::ModelError> {
    let parsed: serde_json::Value = serde_json::from_str(catalogue_body)
        .map_err(|error| runner::ModelError::UnparseableResponse(error.to_string()))?;

    let entry = catalogue_entry(&parsed, model).ok_or_else(|| {
        runner::ModelError::ModelUnavailable(format!("\"{model}\" is not in the model catalogue"))
    })?;

    let window = match entry.get("context_length") {
        // `as_u64` and not `as_i64`: a negative or fractional window is not a smaller window, it is
        // a catalogue this crate cannot read, and `InvalidContextLength` is the name for that —
        // distinct from `MissingContextLength`, which is the catalogue having said nothing at all.
        Some(value) => ContextWindow::Discovered(
            value
                .as_u64()
                .and_then(|tokens| usize::try_from(tokens).ok())
                .ok_or(runner::ModelError::InvalidContextLength)?,
        ),
        None => ContextWindow::Declared(declared.ok_or(runner::ModelError::MissingContextLength)?),
    };

    let (ContextWindow::Discovered(available_tokens) | ContextWindow::Declared(available_tokens)) =
        window;
    if available_tokens < required_tokens {
        return Err(runner::ModelError::ContextTooSmall {
            available_tokens,
            required_tokens,
        });
    }

    Ok(window)
}

/// PURE: the one entry in an OpenAI-compatible `/models` body whose `id` is this model.
///
/// Shared by `resolve_context_window` above and `discover_openai_compatible` below rather than written twice:
/// the window and the capability booleans are read off the SAME entry, and two copies of "find the
/// model" is how one of them ends up matching on a different field than the other.
fn catalogue_entry<'a>(
    parsed: &'a serde_json::Value,
    model: &str,
) -> Option<&'a serde_json::Value> {
    parsed
        .get("data")
        .and_then(serde_json::Value::as_array)
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry.get("id").and_then(serde_json::Value::as_str) == Some(model))
        })
}

/// OpenAI-compatible route: `GET {base_url}/models`, the catalogue half of the dialect `openai_compatible.rs`
/// speaks on `/chat/completions`.
///
/// Named for the WIRE and not for a provider, the same choice `openai_compatible::OpenAiCompatibleChat` makes one module
/// over: the shape is OpenAI's — `{"data": [{"id", "context_length", "supported_parameters", ..}]}`
/// — and OpenRouter is one server that answers it, beside a `llama.cpp`, vLLM or LM Studio on
/// loopback answering the same route. A server states each model's window and parameters in its own
/// catalogue entry rather than answering a per-model probe the way Ollama's `/api/show` does, so
/// there is no request here for `interpret_context_probe` to read; the window decision is
/// `resolve_context_window` above, which this function feeds a body and a fallback and nothing else.
///
/// `declared` is the window an operator stated for a server that publishes none — `None` from the
/// hosted route, which declares nothing locally because OpenRouter's catalogue always states its
/// own. Whichever number wins is logged at `info` WITH its source, because "the declared window is
/// in force" has to be legible to somebody reading the log rather than inferred from a
/// configuration file they do not have in front of them; a silently substituted window is the
/// failure the `Declared` variant exists to make impossible, and the log line is where that
/// distinction becomes visible outside the type system.
///
/// Its first production caller is `assistants.rs`'s `ConfiguredAssistants::declared_one`, for
/// `Brain::OpenRouter` — the `#[cfg_attr(not(test), allow(dead_code))]` this doc used to carry is
/// gone, per its own instruction to delete it "the day the model picker calls it": that day is
/// this one, and a caller that stops reaching this function is a regression clippy should now
/// catch, not one this attribute should keep hiding.
pub async fn discover_openai_compatible(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    required_tokens: usize,
    declared: Option<usize>,
) -> Declared {
    let body = match client.get(format!("{base_url}/models")).send().await {
        Ok(response) => match response.text().await {
            Ok(body) => body,
            Err(error) => {
                return undeclared(runner::ModelError::UnparseableResponse(format!(
                    "could not read the model catalogue response: {error}"
                )));
            }
        },
        Err(error) => {
            return undeclared(runner::ModelError::UnparseableResponse(format!(
                "could not reach the model catalogue: {error}"
            )));
        }
    };

    let window = resolve_context_window(&body, model, declared, required_tokens);
    match window {
        Ok(ContextWindow::Discovered(tokens)) => tracing::info!(
            model = %model,
            source = "discovered",
            tokens,
            "the catalogue published this model's context window"
        ),
        Ok(ContextWindow::Declared(tokens)) => tracing::info!(
            model = %model,
            source = "declared",
            tokens,
            "the catalogue published no context window; the declared one is in force"
        ),
        // A failure is not logged here: it is carried in `Declared.context` and turned into an
        // operator-readable sentence by `missing_capabilities`, which is the one place that wording
        // lives. A second copy at `warn` would say the same thing twice, differently.
        Err(_) => {}
    }

    // Parsed a second time, deliberately: `resolve_context_window` above is PURE and takes a BODY,
    // which is what makes the decision testable with no listener. The capability booleans are a
    // different reading off the same entry, and one extra parse of a catalogue this route fetches
    // once per cache miss is cheaper than handing the pure function a parsed value every caller
    // would then have to produce for it.
    let parsed =
        serde_json::from_str::<serde_json::Value>(&body).unwrap_or(serde_json::Value::Null);
    let entry = catalogue_entry(&parsed, model);

    let supported_parameters = entry
        .map(|entry| string_array(entry, "supported_parameters"))
        .unwrap_or_default();
    let input_modalities = entry
        .and_then(|entry| entry.get("architecture"))
        .map(|architecture| string_array(architecture, "input_modalities"))
        .unwrap_or_default();

    Declared {
        context: window.map(|_| ()),
        tools: supported_parameters
            .iter()
            .any(|parameter| parameter == "tools"),
        vision: input_modalities.iter().any(|modality| modality == "image"),
        structured_output: supported_parameters
            .iter()
            .any(|parameter| parameter == "structured_outputs"),
    }
}

/// PURE: an `/api/tags` body -> the model names in it.
///
/// `/api/tags` is Ollama's list of what this machine has pulled -- the same host and daemon as
/// `/api/show` above, answering a different question: "what can this machine serve" rather than
/// "does this one model meet a requirement". Kept separate from the network read below so the
/// parsing itself is testable with no listener at all, the same split `discover_ollama_as` and
/// `string_array` already keep between fetching a body and reading one.
///
/// Documented shape: `{"models": [{"name": "...", "model": "...", ...}, ...]}`, each entry's
/// `name` read and every other field ignored. **UNVERIFIED against a real Ollama on this
/// machine** -- Ollama was not running when this was written; see
/// `um_ollama_real_lista_os_modelos_que_esta_maquina_tem` below, `#[ignore]`d for the same reason
/// `um_ollama_real_declara_um_array_de_capacidades` is.
///
/// A body this crate cannot parse as JSON, or one with no top-level `models` array, produces an
/// EMPTY list -- never an error and never a partial guess -- matching the fail-closed posture
/// `discover_ollama_as` already takes on an unreadable `/api/show` body.
pub fn interpret_tags(body: &str) -> Vec<String> {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    let Some(models) = value.get("models").and_then(serde_json::Value::as_array) else {
        return Vec::new();
    };
    models
        .iter()
        .filter_map(|entry| entry.get("name").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect()
}

/// What this machine has pulled, over `GET {base_url}/api/tags`.
///
/// Fails closed to an EMPTY list on every kind of failure -- an unreachable daemon, a non-200, a
/// body `interpret_tags` cannot read -- never an error, so a caller merging this into the menu
/// degrades to exactly today's catalogue rather than surfacing a daemon issue nobody asked about.
/// The same posture `get_assistant_models`'s own doc comment argues for the config file, extended
/// here to the network read this packet adds.
///
/// Called from `http.rs`'s `get_assistant_models` and `chosen_brain`, both over the short-timeout
/// `OLLAMA_TAGS_CLIENT` that module builds once and shares -- never a fresh client per request.
pub async fn installed_local_models(client: &reqwest::Client, base_url: &str) -> Vec<String> {
    let response = match client.get(format!("{base_url}/api/tags")).send().await {
        Ok(response) => response,
        Err(_) => return Vec::new(),
    };
    if !response.status().is_success() {
        return Vec::new();
    }
    match response.text().await {
        Ok(body) => interpret_tags(&body),
        Err(_) => Vec::new(),
    }
}

/// How far a download has got, as one frame of `/api/pull` says it.
///
/// `status` is Ollama's own word for what it is doing — `pulling manifest`, `pulling <digest>`,
/// `verifying sha256 digest`, `success` — carried through rather than translated, because this
/// crate does not know the vocabulary and inventing one would mean a frame it had never seen
/// arriving as silence.
///
/// Both counts are `0` on the frames that carry none, which is most of them: only the frames that
/// are actually moving bytes have `completed` and `total`. Zero and not `Option` because the one
/// reader is a percentage, `total == 0` is already the "cannot say yet" case a percentage has to
/// handle, and an `Option` would make it two.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullProgress {
    pub status: String,
    pub completed: u64,
    pub total: u64,
}

/// One readable frame of `/api/pull`'s stream: how far it has got, or why it cannot go on.
///
/// A failure is a frame and not an HTTP error, which is the whole reason this enum exists. Ollama
/// answers `200` and then says `{"error": "..."}` in the body — a model name it does not have, a
/// disk it cannot write — so a reader that checked only the status code would call a download that
/// never happened a success.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PullLine {
    Progress(PullProgress),
    Failed(String),
}

/// PURE: one frame of `/api/pull`'s NDJSON -> what it says about the download.
///
/// `None` for every line that says nothing about it — a blank line between frames, a body this
/// crate cannot parse, an object with neither `error` nor `status`. Skipped and never fatal, the
/// same fail-closed posture `interpret_tags` above takes: one unreadable frame in a stream of
/// thousands must not abandon a download that is otherwise working.
///
/// Documented shape: `{"status": "...", "digest": "...", "total": n, "completed": n}`, with
/// `digest` and everything else ignored, and `total`/`completed` absent on the frames that move no
/// bytes. **UNVERIFIED against a real Ollama** for the same reason `interpret_tags` says so of
/// `/api/tags`; `a_real_ollama_streams_a_pull_it_already_has` is the `#[ignore]`d check.
pub fn interpret_pull(line: &str) -> Option<PullLine> {
    let value = serde_json::from_str::<serde_json::Value>(line).ok()?;
    if let Some(error) = value.get("error").and_then(serde_json::Value::as_str) {
        return Some(PullLine::Failed(error.to_owned()));
    }
    let status = value.get("status").and_then(serde_json::Value::as_str)?;
    Some(PullLine::Progress(PullProgress {
        status: status.to_owned(),
        completed: value
            .get("completed")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
        total: value
            .get("total")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0),
    }))
}

/// PURE: the complete lines in a read buffer, leaving any trailing partial one behind.
///
/// The reason `pull_local_model` below is not a loop over chunks. A multi-gigabyte download is cut
/// into reads wherever the kernel felt like cutting it, mid-object as often as not, and a parser
/// that decoded each read on its own would drop every frame unlucky enough to straddle two — which
/// on a real download is most of them. Everything before the last newline is whole and comes out;
/// everything after it waits for the read that finishes it.
///
/// Lines are trimmed, so a `\r\n` stream and a blank line between frames both arrive as something
/// `interpret_pull` already answers `None` to.
fn take_lines(buffer: &mut Vec<u8>) -> Vec<String> {
    let mut lines = Vec::new();
    while let Some(newline) = buffer.iter().position(|byte| *byte == b'\n') {
        let line: Vec<u8> = buffer.drain(..=newline).collect();
        lines.push(String::from_utf8_lossy(&line).trim().to_owned());
    }
    lines
}

/// Fetch a model this machine does not have, over `POST {base_url}/api/pull`, reporting progress
/// as it goes.
///
/// The one read in this module that does NOT fail closed, and the difference is who asked.
/// `installed_local_models` above answers an empty list on any trouble because nobody asked it
/// anything — it runs to decorate a menu, and a menu that quietly shows fewer models is better than
/// one that shows an error nobody can act on. A pull is something a person asked for and is
/// watching, so every failure is reported: silence would leave a progress bar at zero forever with
/// nothing to say why.
///
/// `progress` is called once per readable frame, in order, from the caller's own task — never
/// buffered and never coalesced, because the caller is what decides where progress goes and how
/// often it is worth storing. It must not block: it runs between reads of a live stream.
///
/// **A stream that simply stops is a failure.** No `success` frame means the download did not
/// finish — a killed Ollama, a full disk, a dropped connection — and every byte that did arrive was
/// valid, so nothing in the loop would otherwise notice. Reporting `Ok` there would leave the
/// window saying a model is downloaded that is not.
///
/// The client is the CALLER's, deliberately, and must not be the short-timeout one
/// `installed_local_models` is given: a pull runs for minutes, and a 2-second timeout would abort
/// every download that was working. See `http::ollama_pull_client`.
/// Ollama's public registry, which serves a model's manifest without a key or an account.
///
/// Separate from `runner::OLLAMA_BASE_URL`: that is the daemon on this machine, this is the shared
/// place it pulls FROM. Asking it what a model weighs is the only way to know before downloading —
/// the local `/api/tags` lists sizes for models already here, which is the question already
/// answered.
pub const OLLAMA_REGISTRY_URL: &str = "https://registry.ollama.ai";

/// PURE: a model name as the registry's own path for its manifest, or `None` when it is not a name.
///
/// This is an ALLOWLIST, not a cleaner, and that matters because the name arrives from a request
/// (`http.rs`) and is interpolated into a path on a host this crate chose. A name carrying `..` or
/// a `?` would aim the read elsewhere on that host, so the first byte of every segment must be
/// alphanumeric and the rest a closed set. There is no repairing branch: a name this does not
/// recognise is not a model, and guessing what somebody meant is how an allowlist becomes a
/// suggestion.
///
/// Ollama's two defaults are applied here rather than at each call site so a bare `qwen3` is spelled
/// the same way everywhere: no tag means `latest`, and no namespace means `library`.
pub fn registry_path(model: &str) -> Option<String> {
    let (repository, tag) = match model.split_once(':') {
        Some((repository, tag)) => (repository, tag),
        None => (model, "latest"),
    };
    let (namespace, name) = match repository.split_once('/') {
        Some((namespace, name)) => (namespace, name),
        None => ("library", repository),
    };
    for segment in [namespace, name, tag] {
        // The leading-byte rule is what rejects `.` and `..` without naming them: a segment that
        // must START alphanumeric cannot be either, nor a dotfile, and real names never are.
        if !segment.bytes().next()?.is_ascii_alphanumeric() {
            return None;
        }
        if !segment
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
        {
            return None;
        }
    }
    Some(format!("{namespace}/{name}/manifests/{tag}"))
}

/// PURE: how many bytes a manifest says a model is, or `None` when it does not say.
///
/// Every layer counts, because every layer is downloaded — the weights are nearly all of it, but
/// the template, system prompt and licence are fetched too and a size that excluded them would be
/// quietly under.
///
/// `None` and never `Some(0)`: zero is a real answer meaning "weighs nothing", and `model_fit`
/// would grade it as the most permissive verdict there is. A failure to read must not arrive at a
/// caller as the best possible news — the same fail-closed posture `interpret_tags` takes on a body
/// it cannot parse.
pub fn interpret_manifest(body: &str) -> Option<u64> {
    let value = serde_json::from_str::<serde_json::Value>(body).ok()?;
    let layers = value.get("layers")?.as_array()?;
    let mut total: u64 = 0;
    for layer in layers {
        total = total.saturating_add(layer.get("size")?.as_u64()?);
    }
    (total > 0).then_some(total)
}

/// What the registry says a model weighs, before a byte of it is downloaded.
///
/// Unlike its neighbours in this module this returns an `Err` rather than failing closed to a quiet
/// nothing: the two readers that fail closed run to decorate a menu nobody asked about, while this
/// one answers a question somebody asked out loud — "how big is it?" — and silence there leaves a
/// download confirmation with nothing to say.
pub async fn registry_model_size(
    client: &reqwest::Client,
    registry_url: &str,
    model: &str,
) -> Result<u64, String> {
    // Before the request, so a refused name costs no connection and cannot be told apart from a
    // real one by how long it took.
    let path = registry_path(model)
        .ok_or_else(|| format!("`{model}` is not a model name the registry could hold"))?;
    let response = client
        .get(format!("{registry_url}/v2/{path}"))
        .send()
        .await
        .map_err(|error| format!("the model registry did not answer: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "the registry does not serve `{model}` ({})",
            response.status()
        ));
    }
    let body = response
        .text()
        .await
        .map_err(|error| format!("the registry's answer could not be read: {error}"))?;
    interpret_manifest(&body)
        .ok_or_else(|| format!("the registry's manifest for `{model}` did not say a size"))
}

/// Whether this machine can carry a model, and how comfortably.
///
/// Three verdicts and an "unknown", because the middle one is the honest answer for most of the
/// interesting range and collapsing it either way is a lie: folded into `Comfortable` it recommends
/// a download that will make the machine crawl, folded into `TooBig` it refuses one that works.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fit {
    /// Runs with room for the machine to keep doing other things.
    Comfortable,
    /// Runs, and takes most of the memory while it does.
    Tight,
    /// Does not fit, and downloading it would spend the bandwidth to prove it.
    TooBig,
    /// Not enough was read to say — an unmeasurable machine or an unreadable manifest.
    Unknown,
}

/// PURE: a model's size against this machine's memory.
///
/// The headroom is a fifth of the weights plus a gigabyte. A model needs its weights resident AND a
/// KV cache that grows with the context it is given, and the machine has to keep running underneath
/// it; without that margin a 15 GB model on a 16 GB machine grades as "fits" and what actually
/// happens is the system swaps until somebody force-quits it. `Comfortable` is then the 70% line,
/// which is where a laptop stops being usable for anything else.
///
/// RAM and not VRAM, deliberately. Ollama falls back to the CPU when a model does not fit on the
/// card, so VRAM answers "will it be fast" while RAM answers "will it run at all" — and this
/// function is on the path of a menu deciding what to offer, which is the second question.
///
/// A zero on either side is `Unknown` rather than a verdict: both mean something could not be read,
/// and turning that into `TooBig` would refuse every download on a machine this crate merely failed
/// to measure.
pub fn model_fit(model_bytes: u64, memory_bytes: u64) -> Fit {
    if model_bytes == 0 || memory_bytes == 0 {
        return Fit::Unknown;
    }
    let needed = model_bytes
        .saturating_add(model_bytes / 5)
        .saturating_add(1_000_000_000);
    if needed > memory_bytes {
        return Fit::TooBig;
    }
    // `memory / 10 * 7` and not `memory * 7 / 10`: the multiply is what would overflow, and on
    // these magnitudes the lost remainder is under ten bytes.
    if needed <= memory_bytes / 10 * 7 {
        Fit::Comfortable
    } else {
        Fit::Tight
    }
}

/// How much physical memory this machine has, or `None` when it cannot be asked.
///
/// One call, no new crate: `windows-sys` is already a dependency for `process_tree.rs`'s job
/// objects, and this adds a feature to it rather than a tree. `TotalPhys` is the installed RAM
/// rather than what is free right now, which is the right number for a menu: what is free changes
/// every second and would make a model appear and disappear from the picker while somebody read it.
///
/// On Unix the same number comes from `sysconf`: `_SC_PHYS_PAGES` times `_SC_PAGESIZE` is the
/// installed RAM, byte for byte what the `total` column of `free -b` reports. `None` only on a
/// target that is neither — and on any failure of the call — because a stub that guessed would be
/// worse than a caller that knows it does not know.
#[cfg(windows)]
pub fn total_memory_bytes() -> Option<u64> {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    // SAFETY: `status` is a live, zeroed `MEMORYSTATUSEX` with its own `dwLength` set, which is the
    // entire contract this call has. It writes only into that struct and takes no ownership.
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return None;
    }
    (status.ullTotalPhys > 0).then_some(status.ullTotalPhys)
}

#[cfg(unix)]
pub fn total_memory_bytes() -> Option<u64> {
    // SAFETY: `sysconf` reads a system constant and takes no pointers.
    let pages = unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) };
    // SAFETY: same contract as above — a constant read, no pointers, no ownership.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    // `sysconf` answers -1 for a name it does not support, and 0 is as unreadable as that.
    if pages <= 0 || page_size <= 0 {
        return None;
    }
    (pages as u64).checked_mul(page_size as u64)
}

#[cfg(not(any(windows, unix)))]
pub fn total_memory_bytes() -> Option<u64> {
    None
}

pub async fn pull_local_model(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    mut progress: impl FnMut(PullProgress),
) -> Result<(), String> {
    let mut response = client
        .post(format!("{base_url}/api/pull"))
        .json(&serde_json::json!({ "model": model, "stream": true }))
        .send()
        .await
        .map_err(|error| format!("Ollama did not answer the download request: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Ollama refused the download with HTTP {}",
            response.status()
        ));
    }

    let mut buffer: Vec<u8> = Vec::new();
    let mut succeeded = false;
    loop {
        match response
            .chunk()
            .await
            .map_err(|error| format!("the download stream broke: {error}"))?
        {
            Some(bytes) => buffer.extend_from_slice(&bytes),
            // The stream ended. A last frame with no trailing newline is still a frame, so it is
            // closed here rather than dropped; the buffer is then empty, and the next read ends the
            // loop for good.
            None if buffer.is_empty() => break,
            None => buffer.push(b'\n'),
        }
        for line in take_lines(&mut buffer) {
            match interpret_pull(&line) {
                Some(PullLine::Failed(reason)) => return Err(reason),
                Some(PullLine::Progress(frame)) => {
                    succeeded |= frame.status == "success";
                    progress(frame);
                }
                None => {}
            }
        }
    }

    if succeeded {
        Ok(())
    } else {
        Err("the download stopped before Ollama said it had finished".to_owned())
    }
}

/// What an agent CLI declares, without touching the network.
///
/// The CLI-wrap invariant (`AGENTS.md`: wrap the existing agent CLI, never reimplement its loop)
/// means an agent CLI's capabilities are a property of the CLI binary itself, not of a served model
/// this daemon could probe — so this is a constant rather than a route, the same reason
/// `hosted_assistant`'s comment in `main.rs` gives for needing no probe of its own.
///
/// Scoped to the non-test build, for the same reason `discover_openai_compatible` above is: no production
/// caller until the model picker packet, exercised under `cfg(test)` by
/// `uma_cli_declara_as_suas_capacidades_sem_tocar_na_rede` in the meantime.
#[cfg_attr(not(test), allow(dead_code))]
pub fn declared_for_cli() -> Declared {
    Declared {
        context: Ok(()),
        tools: true,
        vision: true,
        structured_output: true,
    }
}

/// Caches a model's declared capabilities so a role probes it once, not once per use.
///
/// Keyed by model name because that is the only axis discovery varies on for a fixed route and a
/// fixed requirement: the daemon discovers each configured model once at the point a role first
/// needs it, and a role's own requirement never changes under it while the daemon runs.
///
/// Scoped to the non-test build, for the same reason `discover_openai_compatible` above is: every role
/// `main.rs` wires up today probes once per startup, so none of them needs this cache yet — the
/// model picker packet is the production caller, and this module's own tests already exercise both
/// the single-model and the two-model cases in the meantime.
#[cfg_attr(not(test), allow(dead_code))]
pub struct DiscoveryCache {
    entries: std::sync::Mutex<std::collections::HashMap<String, Declared>>,
}

impl Default for DiscoveryCache {
    fn default() -> Self {
        Self::new()
    }
}

impl DiscoveryCache {
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new() -> Self {
        Self {
            entries: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// Returns the cached declaration for `model`, running `discover` only the first time this
    /// model is asked for. Two models never share an entry: a miss for one model must not read or
    /// write another model's row.
    #[cfg_attr(not(test), allow(dead_code))]
    pub async fn get_or_discover<F, Fut>(&self, model: &str, discover: F) -> Declared
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Declared>,
    {
        if let Some(declared) = self
            .entries
            .lock()
            .expect("discovery cache mutex poisoned")
            .get(model)
        {
            return declared.clone();
        }
        let declared = discover().await;
        self.entries
            .lock()
            .expect("discovery cache mutex poisoned")
            .insert(model.to_string(), declared.clone());
        declared
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::local_agent;
    use crate::triage;
    use crate::voice;

    fn satisfied_requirement() -> Requirement {
        Requirement {
            context_tokens: 8192,
            tools: true,
            vision: true,
            structured_output: true,
        }
    }

    fn fully_declared() -> Declared {
        Declared {
            context: Ok(()),
            tools: true,
            vision: true,
            structured_output: true,
        }
    }

    fn context_too_small(available: usize, required: usize) -> runner::ModelError {
        runner::ModelError::ContextTooSmall {
            available_tokens: available,
            required_tokens: required,
        }
    }

    // ---------------------------------------------------------------------------------------
    // A. The pure comparison
    // ---------------------------------------------------------------------------------------

    #[test]
    fn um_requisito_satisfeito_nao_nomeia_capacidade_nenhuma() {
        let missing = missing_capabilities(&satisfied_requirement(), &fully_declared());
        assert!(
            missing.is_empty(),
            "a fully satisfied requirement must name nothing missing: {missing:?}"
        );
    }

    #[test]
    fn uma_janela_pequena_demais_e_nomeada_com_o_que_ha_e_o_que_e_preciso() {
        let requirement = Requirement {
            context_tokens: 8192,
            tools: false,
            vision: false,
            structured_output: false,
        };
        let declared = Declared {
            context: Err(context_too_small(2048, 8192)),
            tools: false,
            vision: false,
            structured_output: false,
        };

        let missing = missing_capabilities(&requirement, &declared);

        assert_eq!(missing.len(), 1);
        match &missing[0] {
            MissingCapability::ContextWindow(runner::ModelError::ContextTooSmall {
                available_tokens,
                required_tokens,
            }) => {
                assert_eq!(
                    *available_tokens, 2048,
                    "must name what there is: {missing:?}"
                );
                assert_eq!(
                    *required_tokens, 8192,
                    "must name what is needed: {missing:?}"
                );
            }
            other => panic!("expected a named ContextTooSmall gap, got {other:?}"),
        }
    }

    #[test]
    fn um_modelo_que_nao_declara_ferramentas_e_recusado_para_quem_as_exige() {
        let requirement = Requirement {
            context_tokens: 0,
            tools: true,
            vision: false,
            structured_output: false,
        };
        let declared = Declared {
            context: Ok(()),
            tools: false,
            vision: false,
            structured_output: false,
        };

        let missing = missing_capabilities(&requirement, &declared);

        assert!(
            missing.contains(&MissingCapability::Tools),
            "a model that never declared tools must be refused for a requirement that needs them: {missing:?}"
        );
    }

    #[test]
    fn um_modelo_que_nao_declara_visao_e_recusado_para_quem_a_exige() {
        let requirement = Requirement {
            context_tokens: 0,
            tools: false,
            vision: true,
            structured_output: false,
        };
        let declared = Declared {
            context: Ok(()),
            tools: false,
            vision: false,
            structured_output: false,
        };

        let missing = missing_capabilities(&requirement, &declared);

        assert!(
            missing.contains(&MissingCapability::Vision),
            "a model that never declared vision must be refused for a requirement that needs it: {missing:?}"
        );
    }

    #[test]
    fn todas_as_capacidades_em_falta_sao_nomeadas_e_nao_so_a_primeira() {
        let requirement = satisfied_requirement();
        let declared = Declared {
            context: Err(context_too_small(100, 8192)),
            tools: false,
            vision: false,
            structured_output: false,
        };

        let missing = missing_capabilities(&requirement, &declared);

        assert_eq!(
            missing.len(),
            4,
            "every one of the four required capabilities is missing; a short-circuit would stop \
             at the first: {missing:?}"
        );
        assert!(matches!(missing[0], MissingCapability::ContextWindow(_)));
        assert!(missing.contains(&MissingCapability::Tools));
        assert!(missing.contains(&MissingCapability::Vision));
        assert!(missing.contains(&MissingCapability::StructuredOutput));
    }

    #[test]
    fn o_que_a_fonte_nao_declara_conta_como_ausente_e_nao_como_presente() {
        // `context_tokens: 0` -> the window is not under test here; only the three booleans a
        // source can simply omit saying anything about.
        let requirement = Requirement {
            context_tokens: 0,
            tools: true,
            vision: true,
            structured_output: true,
        };
        // A declaration that never mentions any of the three booleans must read as "did not
        // declare them", i.e. false — never as "declared and present".
        let declared = Declared {
            context: Ok(()),
            tools: false,
            vision: false,
            structured_output: false,
        };

        let missing = missing_capabilities(&requirement, &declared);

        assert!(missing.contains(&MissingCapability::Tools));
        assert!(missing.contains(&MissingCapability::Vision));
        assert!(missing.contains(&MissingCapability::StructuredOutput));
    }

    // ---------------------------------------------------------------------------------------
    // B. Discovery per route
    // ---------------------------------------------------------------------------------------

    /// A loopback Ollama `/api/show`, answering the same canned body every time. Bound on
    /// `127.0.0.1:0` and served with `axum::serve`, the idiom `runner.rs`'s own
    /// `ollama_runner_capturing` and `pii_shadow.rs`'s `stub_ollama_capturing` already use for this
    /// exact route — no mock-HTTP crate exists in this crate's dev-dependencies and none is added
    /// here.
    async fn stub_ollama_show(body: serde_json::Value) -> String {
        let app = axum::Router::new().route(
            "/api/show",
            axum::routing::post(move || {
                let body = body.clone();
                async move { axum::Json(body) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{address}")
    }

    /// A loopback Ollama `/api/show` answering with a body the JSON parser itself refuses — plain
    /// text with no structure at all, the same failure `interpret_context_probe` already names
    /// `UnparseableResponse`.
    async fn stub_ollama_show_unreadable() -> String {
        let app = axum::Router::new().route(
            "/api/show",
            axum::routing::post(|| async { "not json, not anything" }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{address}")
    }

    /// A loopback OpenRouter model catalogue, answering `GET /models` with one entry.
    async fn stub_openrouter_models(entry: serde_json::Value) -> String {
        let app = axum::Router::new().route(
            "/models",
            axum::routing::get(move || {
                let entry = entry.clone();
                async move { axum::Json(serde_json::json!({ "data": [entry] })) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn a_descoberta_le_a_janela_de_uma_resposta_do_api_show() {
        let base_url = stub_ollama_show(serde_json::json!({
            "model_info": {
                "general.architecture": "qwen2",
                "qwen2.context_length": 32768
            },
            "capabilities": ["completion"]
        }))
        .await;
        let client = reqwest::Client::new();

        let declared = discover_ollama(&client, &base_url, "qwen2", 8192).await;

        assert_eq!(
            declared.context,
            Ok(()),
            "a window of 32768 must satisfy a requirement of 8192: {declared:?}"
        );
    }

    #[tokio::test]
    async fn a_descoberta_le_as_ferramentas_do_array_de_capacidades_do_ollama() {
        let base_url = stub_ollama_show(serde_json::json!({
            "model_info": {
                "general.architecture": "qwen2",
                "qwen2.context_length": 32768
            },
            "capabilities": ["completion", "tools"]
        }))
        .await;
        let client = reqwest::Client::new();

        let declared = discover_ollama(&client, &base_url, "qwen2", 8192).await;

        assert!(
            declared.tools,
            "\"tools\" is in the capabilities array and must be read as declared: {declared:?}"
        );
        assert!(
            !declared.vision,
            "\"vision\" is absent from the array and must not be inferred true: {declared:?}"
        );
    }

    #[tokio::test]
    async fn a_descoberta_le_a_janela_e_as_ferramentas_do_catalogo_do_openrouter() {
        let base_url = stub_openrouter_models(serde_json::json!({
            "id": "anthropic/claude-sonnet-4.5",
            "context_length": 200000,
            "supported_parameters": ["tools", "temperature"],
            "architecture": {"input_modalities": ["text"]}
        }))
        .await;
        let client = reqwest::Client::new();

        let declared = discover_openai_compatible(
            &client,
            &base_url,
            "anthropic/claude-sonnet-4.5",
            8192,
            None,
        )
        .await;

        assert_eq!(
            declared.context,
            Ok(()),
            "the catalogue states a 200000-token window, which must satisfy 8192: {declared:?}"
        );
        assert!(
            declared.tools,
            "\"tools\" is in supported_parameters and must be read as declared: {declared:?}"
        );
        assert!(
            !declared.vision,
            "the entry's input_modalities carries no \"image\" and must not declare vision: {declared:?}"
        );
    }

    #[tokio::test]
    async fn uma_cli_declara_as_suas_capacidades_sem_tocar_na_rede() {
        // No listener bound, no client passed in: `declared_for_cli` must not need either.
        let declared = declared_for_cli();

        assert_eq!(
            declared.context,
            Ok(()),
            "an agent CLI manages its own context and never fails this module's probe: {declared:?}"
        );
        assert!(
            declared.tools,
            "an agent CLI's whole role is calling tools: {declared:?}"
        );
    }

    #[tokio::test]
    async fn uma_resposta_ilegivel_nao_declara_capacidade_nenhuma() {
        let base_url = stub_ollama_show_unreadable().await;
        let client = reqwest::Client::new();

        let declared = discover_ollama(&client, &base_url, "qwen2", 8192).await;

        assert!(
            matches!(
                declared.context,
                Err(runner::ModelError::UnparseableResponse(_))
            ),
            "an unreadable response must fail the window closed, not silently pass it: {declared:?}"
        );
        assert!(
            !declared.tools,
            "an unreadable response declares no capability at all: {declared:?}"
        );
        assert!(
            !declared.vision,
            "an unreadable response declares no capability at all: {declared:?}"
        );
        assert!(
            !declared.structured_output,
            "an unreadable response declares no capability at all: {declared:?}"
        );
    }

    // ---------------------------------------------------------------------------------------
    // C. Cache
    // ---------------------------------------------------------------------------------------

    #[tokio::test]
    async fn a_descoberta_corre_uma_vez_por_modelo_e_nao_uma_vez_por_utilizacao() {
        let base_url = stub_ollama_show(serde_json::json!({
            "model_info": {
                "general.architecture": "qwen2",
                "qwen2.context_length": 32768
            },
            "capabilities": ["completion", "tools"]
        }))
        .await;
        let client = reqwest::Client::new();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cache = DiscoveryCache::new();

        for _ in 0..2 {
            let calls = calls.clone();
            let client = client.clone();
            let base_url = base_url.clone();
            cache
                .get_or_discover("qwen2", || async move {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    discover_ollama(&client, &base_url, "qwen2", 8192).await
                })
                .await;
        }

        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "two lookups of the same model must probe the network once, not twice"
        );
    }

    #[tokio::test]
    async fn dois_modelos_sao_descobertos_um_de_cada_vez_e_nao_partilham_resposta() {
        let base_url = stub_ollama_show_by_model().await;
        let client = reqwest::Client::new();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let cache = DiscoveryCache::new();

        let discover_for = |model: &'static str| {
            let calls = calls.clone();
            let client = client.clone();
            let base_url = base_url.clone();
            async move {
                calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                discover_ollama(&client, &base_url, model, 8192).await
            }
        };

        let alfa = cache.get_or_discover("alfa", || discover_for("alfa")).await;
        let beta = cache.get_or_discover("beta", || discover_for("beta")).await;

        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "two DIFFERENT models must each be discovered once, not merged into one lookup"
        );
        assert_ne!(
            alfa.tools, beta.tools,
            "alfa and beta must not share one cached response: {alfa:?} vs {beta:?}"
        );
    }

    /// A loopback Ollama `/api/show` whose answer depends on the `model` field of the posted body —
    /// `alfa` declares tools, `beta` does not — so a cache bug that let two different models read
    /// each other's entry is observable as the wrong boolean, not just as a wrong call count.
    async fn stub_ollama_show_by_model() -> String {
        let app = axum::Router::new().route(
            "/api/show",
            axum::routing::post(
                |axum::Json(body): axum::Json<serde_json::Value>| async move {
                    let declares_tools =
                        body.get("model").and_then(serde_json::Value::as_str) == Some("alfa");
                    let capabilities: Vec<&str> = if declares_tools {
                        vec!["completion", "tools"]
                    } else {
                        vec!["completion"]
                    };
                    axum::Json(serde_json::json!({
                        "model_info": {
                            "general.architecture": "qwen2",
                            "qwen2.context_length": 32768
                        },
                        "capabilities": capabilities
                    }))
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{address}")
    }

    // ---------------------------------------------------------------------------------------
    // D. The three postures
    // ---------------------------------------------------------------------------------------

    #[test]
    fn a_triagem_desliga_por_completo_e_diz_porque() {
        let error = context_too_small(2048, triage::LOCAL_NUM_CTX);
        let missing = vec![MissingCapability::ContextWindow(error.clone())];

        let posture = triage_posture(&missing);

        let expected_reason = match runner::local_triage_decision(Err(error)) {
            runner::LocalTriageDecision::Disabled(reason) => reason,
            runner::LocalTriageDecision::Enabled => panic!("an error must decide Disabled"),
        };
        assert_eq!(
            posture,
            Posture::SwitchedOff(expected_reason),
            "triage's reason must be runner::local_triage_decision's own words, not a second copy"
        );
    }

    #[test]
    fn a_voz_desliga_o_polimento_e_o_ditado_segue_em_cru() {
        let error = context_too_small(1024, voice::CLEANUP_NUM_CTX);
        let missing = vec![MissingCapability::ContextWindow(error)];

        let posture = voice_posture(&missing);

        assert!(
            matches!(posture, Posture::Degraded(_)),
            "voice must degrade, never switch off entirely: {posture:?}"
        );
    }

    #[test]
    fn a_conversa_local_cai_para_a_cli_em_vez_de_desligar() {
        let error = context_too_small(4096, local_agent::TURN_NUM_CTX);
        let missing = vec![MissingCapability::ContextWindow(error)];

        let posture = local_assistant_posture(&missing);

        assert!(
            matches!(posture, Posture::FellBackToCli(_)),
            "the local assistant must fall back to the CLI, never switch off: {posture:?}"
        );
    }

    #[test]
    fn as_tres_posturas_sao_distintas_e_nenhuma_herda_a_do_vizinho() {
        // The SAME failure, handed to all three roles.
        let error = context_too_small(512, 8192);
        let missing = vec![MissingCapability::ContextWindow(error)];

        let triage_outcome = triage_posture(&missing);
        let voice_outcome = voice_posture(&missing);
        let local_assistant_outcome = local_assistant_posture(&missing);

        assert!(matches!(triage_outcome, Posture::SwitchedOff(_)));
        assert!(matches!(voice_outcome, Posture::Degraded(_)));
        assert!(matches!(local_assistant_outcome, Posture::FellBackToCli(_)));

        // A refactor that collapsed the three into one shared outcome would make at least two of
        // these three equal; none of the three may be.
        assert_ne!(triage_outcome, voice_outcome);
        assert_ne!(voice_outcome, local_assistant_outcome);
        assert_ne!(triage_outcome, local_assistant_outcome);
    }

    #[test]
    fn um_requisito_cumprido_deixa_os_tres_papeis_exactamente_como_estavam() {
        let missing: Vec<MissingCapability> = Vec::new();

        assert_eq!(triage_posture(&missing), Posture::Unaffected);
        assert_eq!(voice_posture(&missing), Posture::Unaffected);
        assert_eq!(local_assistant_posture(&missing), Posture::Unaffected);
    }

    // ---------------------------------------------------------------------------------------
    // E. What each role declares
    // ---------------------------------------------------------------------------------------

    #[test]
    fn a_triagem_exige_a_janela_que_o_triage_rs_ja_fixava() {
        assert_eq!(
            triage::CAPABILITY_REQUIREMENT.context_tokens,
            triage::LOCAL_NUM_CTX,
            "triage's requirement must mirror the constant triage.rs already fixed"
        );
    }

    #[test]
    fn a_voz_exige_a_janela_que_o_voice_rs_ja_fixava() {
        assert_eq!(
            voice::CAPABILITY_REQUIREMENT.context_tokens,
            voice::CLEANUP_NUM_CTX,
            "voice's requirement must mirror the constant voice.rs already fixed"
        );
    }

    #[test]
    fn a_conversa_local_exige_a_janela_que_o_local_agent_rs_ja_fixava() {
        assert_eq!(
            local_agent::CAPABILITY_REQUIREMENT.context_tokens,
            local_agent::TURN_NUM_CTX,
            "the local assistant's requirement must mirror the constant local_agent.rs already fixed"
        );
    }

    #[test]
    fn nenhum_dos_tres_papeis_exige_hoje_mais_do_que_a_janela() {
        // The guard: if a future change adds a requirement to any of the three roles beyond the
        // window they already enforce, THIS test must fail and make that change say so out loud,
        // instead of the model picker silently starting to disable local models it did not used to.
        assert_eq!(
            triage::CAPABILITY_REQUIREMENT,
            Requirement {
                context_tokens: triage::LOCAL_NUM_CTX,
                tools: false,
                vision: false,
                structured_output: false,
            }
        );
        assert_eq!(
            voice::CAPABILITY_REQUIREMENT,
            Requirement {
                context_tokens: voice::CLEANUP_NUM_CTX,
                tools: false,
                vision: false,
                structured_output: false,
            }
        );
        assert_eq!(
            local_agent::CAPABILITY_REQUIREMENT,
            Requirement {
                context_tokens: local_agent::TURN_NUM_CTX,
                tools: false,
                vision: false,
                structured_output: false,
            }
        );
    }

    // ---------------------------------------------------------------------------------------
    // F. Against the real thing
    // ---------------------------------------------------------------------------------------

    /// The one thing every Ollama fixture above could not be checked against on this machine:
    /// whether a real `/api/show` really answers with a top-level `capabilities` array the way
    /// `discover_ollama` assumes. Ollama was not running here when this module was written; the
    /// OpenRouter half of discovery was checked against the live catalogue instead (see the
    /// packet this module shipped under). `#[ignore]` because it needs a running Ollama at
    /// `runner::OLLAMA_BASE_URL` with the configured local model already pulled — the same
    /// real-world caveat `voice.rs`'s `real_pipeline_transcribes_and_cleans_an_actual_recording`
    /// states for its own pipeline, and the reason this repository prefers one such test over
    /// twenty against a double.
    ///
    /// Run it deliberately, from the repository root, with Ollama already running:
    ///   cargo test -p nucleos-core -- --ignored --nocapture capabilities::tests::um_ollama_real_declara_um_array_de_capacidades
    #[tokio::test]
    #[ignore = "needs a running Ollama at runner::OLLAMA_BASE_URL with the configured local model pulled"]
    async fn um_ollama_real_declara_um_array_de_capacidades() {
        let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("core/ has a parent");
        let models = crate::config::load_models_config(&repo.join(".ai/nucleos-models.yaml"))
            .expect("the daemon's own models config must parse");
        let model = models
            .local_triage_model
            .clone()
            .expect("`local_triage_model` must be set for this test to mean anything");

        let body = reqwest::Client::new()
            .post(format!("{}/api/show", runner::OLLAMA_BASE_URL))
            .json(&serde_json::json!({ "model": &model }))
            .send()
            .await
            .expect("a running Ollama should answer /api/show")
            .text()
            .await
            .expect("the response body should be readable");

        let parsed: serde_json::Value =
            serde_json::from_str(&body).expect("/api/show should answer JSON");

        assert!(
            parsed
                .get("capabilities")
                .and_then(serde_json::Value::as_array)
                .is_some(),
            "a real Ollama's /api/show must carry a top-level capabilities array: {parsed:?}"
        );
    }

    // ---------------------------------------------------------------------------------------
    // G. The two reqwest error branches nothing above reaches
    // ---------------------------------------------------------------------------------------

    /// A loopback address claimed and released before the probe ever dials it: the listener binds
    /// to get a real, unused port, then is dropped without accepting anything, so a connect attempt
    /// moments later finds nobody there — a connection failure, not a response of any shape, and
    /// the reqwest error branch none of the tests above reach (`uma_resposta_ilegivel_...` gets a
    /// response and fails on its *body*; this one never gets a response at all).
    #[tokio::test]
    async fn uma_falha_de_ligacao_nao_declara_capacidade_nenhuma() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let base_url = format!("http://{address}");
        let client = reqwest::Client::new();

        let declared = discover_ollama(&client, &base_url, "qwen2", 8192).await;

        match &declared.context {
            Err(runner::ModelError::UnparseableResponse(message)) => {
                assert!(
                    message.contains("could not reach the loopback Ollama endpoint"),
                    "a connect failure must carry the endpoint-unreachable wording, never the \
                     probe-read one meant for a body that was actually received: {message}"
                );
            }
            other => panic!(
                "a probe against nothing listening must fail closed as UnparseableResponse: {other:?}"
            ),
        }
        assert!(
            !declared.tools,
            "a connect failure declares no capability at all: {declared:?}"
        );
        assert!(
            !declared.vision,
            "a connect failure declares no capability at all: {declared:?}"
        );
        assert!(
            !declared.structured_output,
            "a connect failure declares no capability at all: {declared:?}"
        );
    }

    /// A loopback `/api/show` that accepts the connection and answers, but promises more body than
    /// it ever sends — `Content-Length: 1000` followed by five bytes and a closed socket — so
    /// `response.text().await` fails the way `discover_ollama_as`'s middle branch is written for,
    /// genuinely, rather than through the connect-failure substitute this test's doc comment on the
    /// send-back packet allowed as a fallback. Raw `TcpStream` because `axum::serve`, used by every
    /// other stub in this module, always writes a `Content-Length` that matches what it sends and
    /// cannot be told to lie about its own body.
    async fn stub_ollama_show_truncated() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            if let Ok((mut stream, _)) = listener.accept().await {
                use tokio::io::{AsyncReadExt, AsyncWriteExt};
                let mut buf = [0u8; 1024];
                // Drain whatever the client sent; the canned response does not depend on it.
                let _ = stream.read(&mut buf).await;
                let _ = stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\nshort")
                    .await;
                // Dropping here closes the connection well before the promised 1000 bytes arrive.
            }
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn cada_papel_se_nomeia_a_si_proprio_quando_a_resposta_nao_pode_ser_lida() {
        // The exact three labels `main.rs` passes at its three call sites — this is the send-back
        // this test exists to close: a unification that goes back to one shared string here would
        // fail this loop on whichever label it collapsed into the others, instead of shipping a
        // silent wording change on an operator-facing path.
        for probe in ["local model", "voice cleanup", "local assistant"] {
            let base_url = stub_ollama_show_truncated().await;
            let client = reqwest::Client::new();

            let declared = discover_ollama_as(&client, &base_url, "qwen2", 8192, probe).await;

            match &declared.context {
                Err(runner::ModelError::UnparseableResponse(message)) => {
                    assert!(
                        message.contains(&format!("could not read the {probe} probe response")),
                        "the \"{probe}\" call site must name itself in its own probe-read \
                         failure, not borrow another role's wording: {message}"
                    );
                }
                other => panic!(
                    "expected an UnparseableResponse naming \"{probe}\" for a truncated body: {other:?}"
                ),
            }
        }
    }

    // ---------------------------------------------------------------------------------------
    // H. `/api/tags` -- the models this machine has installed
    // ---------------------------------------------------------------------------------------

    /// A loopback Ollama `/api/tags`, answering the documented shape for a fixed list of model
    /// names -- `{"models": [{"name": "...", ...}]}`. Same idiom `stub_ollama_show` above uses;
    /// **the shape itself is UNVERIFIED**, per `interpret_tags`'s own doc comment.
    async fn stub_ollama_tags(names: &[&str]) -> String {
        let entries: Vec<serde_json::Value> = names
            .iter()
            .map(|name| serde_json::json!({ "name": name, "model": name }))
            .collect();
        let app = axum::Router::new().route(
            "/api/tags",
            axum::routing::get(move || {
                let entries = entries.clone();
                async move { axum::Json(serde_json::json!({ "models": entries })) }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{address}")
    }

    #[tokio::test]
    async fn a_leitura_do_api_tags_devolve_os_nomes_dos_modelos_instalados() {
        let base_url = stub_ollama_tags(&["qwen3.5:4b", "llama3.2:3b"]).await;
        let client = reqwest::Client::new();

        let installed = installed_local_models(&client, &base_url).await;

        assert!(
            installed.contains(&"qwen3.5:4b".to_string()),
            "{installed:?}"
        );
        assert!(
            installed.contains(&"llama3.2:3b".to_string()),
            "{installed:?}"
        );
    }

    /// PURE, no listener: every shape `interpret_tags` must fail closed on rather than partially
    /// guess -- not JSON at all, JSON with no `models` key, and a `models` key that is not an
    /// array.
    #[test]
    fn um_api_tags_ilegivel_nao_declara_modelo_nenhum() {
        for body in [
            "not json, not anything",
            "{}",
            r#"{"other_key": []}"#,
            r#"{"models": "not an array"}"#,
        ] {
            let installed = interpret_tags(body);
            assert!(
                installed.is_empty(),
                "a body of this shape must declare no installed model, never a partial guess: \
                 {body:?} -> {installed:?}"
            );
        }
    }

    /// The same connect-failure shape `uma_falha_de_ligacao_nao_declara_capacidade_nenhuma` above
    /// exercises for `/api/show`: a loopback port claimed and released before the read ever dials
    /// it, so there is nobody there to answer at all.
    #[tokio::test]
    async fn um_ollama_inalcancavel_nao_declara_modelo_nenhum() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let base_url = format!("http://{address}");
        let client = reqwest::Client::new();

        let installed = installed_local_models(&client, &base_url).await;

        assert!(
            installed.is_empty(),
            "an unreachable Ollama must fail closed to no installed models, never an error and \
             never a guess: {installed:?}"
        );
    }

    /// The one thing every fixture above could not check on this machine: whether a real
    /// `/api/tags` really answers with the documented `{"models": [{"name": ...}]}` shape. Ollama
    /// was not running here when this module was written. `#[ignore]`, exactly like
    /// `um_ollama_real_declara_um_array_de_capacidades` above and for the same reason.
    ///
    /// Run it deliberately, from the repository root, with Ollama already running:
    ///   cargo test -p nucleos-core -- --ignored --nocapture capabilities::tests::um_ollama_real_lista_os_modelos_que_esta_maquina_tem
    #[tokio::test]
    #[ignore = "needs a running Ollama at runner::OLLAMA_BASE_URL"]
    async fn um_ollama_real_lista_os_modelos_que_esta_maquina_tem() {
        let client = reqwest::Client::new();

        let installed = installed_local_models(&client, runner::OLLAMA_BASE_URL).await;

        assert!(
            !installed.is_empty(),
            "a machine with Ollama running should have at least one model pulled: {installed:?}"
        );
    }

    // ---------------------------------------------------------------------------------------
    // I. The picker's refusal — `picker_refusal`, one sentence naming every gap
    // ---------------------------------------------------------------------------------------

    #[test]
    fn um_modelo_que_serve_nao_da_recusa_nenhuma_no_picker() {
        let missing: Vec<MissingCapability> = Vec::new();

        assert_eq!(
            picker_refusal(&missing),
            None,
            "a model with nothing missing must not be refused by the picker"
        );
    }

    #[test]
    fn uma_janela_pequena_demais_e_recusada_no_picker_com_os_dois_numeros() {
        let missing = vec![MissingCapability::ContextWindow(context_too_small(
            2048, 8192,
        ))];

        let refusal =
            picker_refusal(&missing).expect("a missing context window must refuse at the picker");

        assert!(
            refusal.contains("2048"),
            "the sentence must name what the model has: {refusal}"
        );
        assert!(
            refusal.contains("8192"),
            "the sentence must name what the role needs: {refusal}"
        );
    }

    #[test]
    fn a_recusa_do_picker_nomeia_todas_as_capacidades_em_falta_e_nao_so_a_primeira() {
        let requirement = satisfied_requirement();
        let declared = Declared {
            context: Err(context_too_small(100, 8192)),
            tools: false,
            vision: false,
            structured_output: false,
        };
        let missing = missing_capabilities(&requirement, &declared);
        assert_eq!(
            missing.len(),
            4,
            "the fixture must miss all four: {missing:?}"
        );

        let refusal = picker_refusal(&missing).expect("four gaps must refuse at the picker");

        assert!(
            refusal.contains("100") && refusal.contains("8192"),
            "the context gap's own two numbers must survive alongside the others: {refusal}"
        );
        assert!(
            refusal.contains("tools"),
            "the missing tools capability must be named, not dropped after the first gap: {refusal}"
        );
        assert!(
            refusal.contains("vision"),
            "the missing vision capability must be named, not dropped after the first gap: {refusal}"
        );
        assert!(
            refusal.contains("structured output"),
            "the missing structured-output capability must be named, not dropped after the first \
             gap: {refusal}"
        );
    }

    // ---------------------------------------------------------------------------------------
    // J. `/api/pull` — fetching a model this machine does not have yet
    // ---------------------------------------------------------------------------------------

    /// A loopback Ollama `/api/pull` answering a fixed NDJSON body.
    ///
    /// Same idiom `stub_ollama_tags` above uses. It does NOT control how the body is cut into
    /// reads — nothing at this level can, because that is the kernel's decision and not the
    /// fixture's — so the property that a frame survives being split is pinned where it can be
    /// pinned exactly: `a_frame_split_across_two_reads_is_taken_only_once_it_is_whole`, on
    /// `take_lines` itself, with no listener at all.
    ///
    /// **The shape is UNVERIFIED against a real Ollama**, exactly as `stub_ollama_tags` says of
    /// `/api/tags`, and for the same reason — see `a_real_ollama_streams_a_pull_it_already_has` at
    /// the end of this section, `#[ignore]`d like its two siblings.
    async fn stub_ollama_pull(body: &'static str) -> String {
        let app = axum::Router::new().route(
            "/api/pull",
            axum::routing::post(move || async move { body }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{address}")
    }

    /// PURE: the buffering itself, which is the only part of this read that a network fixture
    /// cannot pin.
    ///
    /// `/api/pull` is the first STREAMING read in this module, and a multi-gigabyte download is cut
    /// into reads wherever the kernel felt like cutting it — mid-object as often as not. A parser
    /// that decoded each read on its own would drop every frame unlucky enough to straddle two,
    /// which on a real download is most of them.
    ///
    /// The trailing partial line staying in the buffer is the whole mechanism, so it is asserted
    /// rather than implied: after the first read the buffer must still hold the half-frame, and
    /// after the second that frame must come out whole and once.
    #[test]
    fn a_frame_split_across_two_reads_is_taken_only_once_it_is_whole() {
        let mut buffer: Vec<u8> = Vec::new();

        buffer.extend_from_slice(b"{\"status\":\"pulling manifest\"}\n{\"status\":\"pulling 89");
        assert_eq!(
            take_lines(&mut buffer),
            vec!["{\"status\":\"pulling manifest\"}".to_string()],
            "only the whole frame comes out of the first read"
        );
        assert!(
            !buffer.is_empty(),
            "the half-frame must stay in the buffer rather than be parsed or dropped"
        );

        buffer.extend_from_slice(b"34\",\"completed\":50}\n");
        assert_eq!(
            take_lines(&mut buffer),
            vec!["{\"status\":\"pulling 8934\",\"completed\":50}".to_string()],
            "the frame comes out whole once its second half arrives, and only once"
        );
        assert!(
            buffer.is_empty(),
            "a buffer whose last byte was a newline holds nothing back"
        );
    }

    /// PURE, no listener: one documented progress frame -> the three things a progress bar needs.
    #[test]
    fn a_pull_line_says_what_is_happening_and_how_far_it_has_got() {
        let line = r#"{"status":"pulling 8934d96d3f08","digest":"sha256:8934","total":2142590208,"completed":241970}"#;

        match interpret_pull(line) {
            Some(PullLine::Progress(progress)) => {
                assert_eq!(progress.status, "pulling 8934d96d3f08");
                assert_eq!(progress.completed, 241_970);
                assert_eq!(progress.total, 2_142_590_208);
            }
            other => panic!("a documented progress frame must read as progress: {other:?}"),
        }
    }

    /// PURE: the two frames that carry no byte counts at all, which is most of the stream.
    ///
    /// `pulling manifest` opens every pull and `success` closes it, and neither has `completed` or
    /// `total`. Reading an absent count as anything but zero would make the opening frame of every
    /// download a wild percentage.
    #[test]
    fn a_pull_line_with_no_counts_is_progress_at_zero_and_not_a_failure() {
        for (line, status) in [
            (r#"{"status":"pulling manifest"}"#, "pulling manifest"),
            (r#"{"status":"success"}"#, "success"),
        ] {
            match interpret_pull(line) {
                Some(PullLine::Progress(progress)) => {
                    assert_eq!(progress.status, status);
                    assert_eq!(progress.completed, 0, "{line}");
                    assert_eq!(progress.total, 0, "{line}");
                }
                other => panic!("{line} must read as progress with no counts: {other:?}"),
            }
        }
    }

    /// PURE: the shape Ollama uses to say the pull cannot happen — a name it does not have, a disk
    /// it cannot write.
    ///
    /// It is a 200 carrying an `error` key, not an HTTP failure, so a reader that checked only the
    /// status would call a download that never happened a success.
    #[test]
    fn a_pull_line_carrying_an_error_is_a_failure_and_not_progress() {
        let line = r#"{"error":"pull model manifest: file does not exist"}"#;

        match interpret_pull(line) {
            Some(PullLine::Failed(reason)) => assert!(
                reason.contains("file does not exist"),
                "the reason Ollama gave must survive verbatim: {reason}"
            ),
            other => panic!("an error frame must read as a failure: {other:?}"),
        }
    }

    /// PURE: every shape that is not a frame of this stream at all.
    ///
    /// Skipped rather than guessed and never fatal — the same fail-closed posture `interpret_tags`
    /// takes. A blank line between frames is ordinary, and one unreadable frame in a stream of
    /// thousands must not abandon a download that is otherwise working.
    #[test]
    fn an_unreadable_pull_line_is_skipped_rather_than_guessed() {
        for line in ["", "   ", "not json at all", "[]", r#"{"other_key": 1}"#] {
            assert!(
                interpret_pull(line).is_none(),
                "a line of this shape says nothing about the download: {line:?}"
            );
        }
    }

    /// The whole read, over a listener: every frame reported once, in order, and then success.
    ///
    /// The final frame carries no counts, which is what Ollama really sends — so this also pins
    /// that the last thing a watcher hears is `success` and not the last percentage before it.
    #[tokio::test]
    async fn a_pull_reports_every_frame_and_then_succeeds() {
        let base_url = stub_ollama_pull(concat!(
            "{\"status\":\"pulling manifest\"}\n",
            "{\"status\":\"pulling 8934\",\"completed\":50,\"total\":100}\n",
            "{\"status\":\"success\"}\n",
        ))
        .await;
        let client = reqwest::Client::new();
        let mut seen: Vec<(String, u64, u64)> = Vec::new();

        let outcome = pull_local_model(&client, &base_url, "qwen3.5:4b", |progress| {
            seen.push((progress.status, progress.completed, progress.total));
        })
        .await;

        assert_eq!(outcome, Ok(()), "a stream ending in success must succeed");
        assert_eq!(
            seen,
            vec![
                ("pulling manifest".to_string(), 0, 0),
                ("pulling 8934".to_string(), 50, 100),
                ("success".to_string(), 0, 0),
            ],
            "every frame, once, in the order Ollama sent them"
        );
    }

    /// A stream that ends in an `error` frame fails, and fails with what Ollama said.
    ///
    /// The reason is carried out verbatim rather than replaced with wording of this crate's own,
    /// because the person reading it is being told why a download they asked for did not happen,
    /// and Ollama is the only party here that knows.
    #[tokio::test]
    async fn a_pull_ollama_refuses_fails_with_the_reason_ollama_gave() {
        let base_url = stub_ollama_pull(concat!(
            "{\"status\":\"pulling manifest\"}\n",
            "{\"error\":\"pull model manifest: file does not exist\"}\n",
        ))
        .await;
        let client = reqwest::Client::new();

        let outcome = pull_local_model(&client, &base_url, "nope:1b", |_| {}).await;

        match outcome {
            Err(reason) => assert!(
                reason.contains("file does not exist"),
                "the failure must name what Ollama said: {reason}"
            ),
            Ok(()) => panic!("a stream carrying an error frame must not report success"),
        }
    }

    /// A stream that simply stops — no `success`, no `error` — is a failure and not a quiet
    /// success.
    ///
    /// This is what a killed Ollama, a full disk or a dropped connection looks like from here, and
    /// it is the one failure that would otherwise be invisible: every byte that did arrive was
    /// valid, so nothing in the loop noticed. Reporting `Ok` would leave the window saying a model
    /// is downloaded that is not, which is worse than any error message.
    #[tokio::test]
    async fn a_pull_whose_stream_stops_short_is_a_failure_and_not_a_quiet_success() {
        let base_url = stub_ollama_pull(concat!(
            "{\"status\":\"pulling manifest\"}\n",
            "{\"status\":\"pulling 8934\",\"completed\":50,\"total\":100}\n",
        ))
        .await;
        let client = reqwest::Client::new();

        let outcome = pull_local_model(&client, &base_url, "qwen3.5:4b", |_| {}).await;

        assert!(
            outcome.is_err(),
            "a stream that never said success must not be reported as one: {outcome:?}"
        );
    }

    /// The same connect-failure shape the `/api/tags` reads above are held to, and the one place
    /// this function's posture DIFFERS from theirs: a pull that cannot dial reports an error.
    ///
    /// `installed_local_models` fails closed to an empty list because nobody asked it anything — it
    /// runs to decorate a menu. A pull is something a person asked for and is watching, so silence
    /// would leave a progress bar at zero forever with nothing to say why.
    #[tokio::test]
    async fn a_pull_that_cannot_reach_ollama_says_so_rather_than_failing_closed() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        drop(listener);
        let base_url = format!("http://{address}");
        let client = reqwest::Client::new();

        let outcome = pull_local_model(&client, &base_url, "qwen3.5:4b", |_| {}).await;

        assert!(
            outcome.is_err(),
            "an unreachable Ollama must be reported, not swallowed: {outcome:?}"
        );
    }

    /// The one thing no fixture above can check: whether a real `/api/pull` really streams the
    /// shape every test in this section assumes.
    ///
    /// Asks for a model this machine already has, deliberately — Ollama answers such a pull in
    /// milliseconds with the same frames, so the check costs no bandwidth and downloads nothing.
    /// `#[ignore]`, exactly like `um_ollama_real_lista_os_modelos_que_esta_maquina_tem` above and
    /// for the same reason: Ollama is not running on every machine this suite runs on.
    #[tokio::test]
    #[ignore = "needs a running Ollama at runner::OLLAMA_BASE_URL with at least one model pulled"]
    async fn a_real_ollama_streams_a_pull_it_already_has() {
        let client = reqwest::Client::new();
        let installed = installed_local_models(&client, crate::runner::OLLAMA_BASE_URL).await;
        let model = installed
            .first()
            .expect("this test needs a machine with at least one model already pulled")
            .clone();
        let mut seen: Vec<String> = Vec::new();

        let outcome = pull_local_model(
            &client,
            crate::runner::OLLAMA_BASE_URL,
            &model,
            |progress| {
                seen.push(progress.status);
            },
        )
        .await;

        assert_eq!(
            outcome,
            Ok(()),
            "re-pulling a model already here must succeed"
        );
        assert!(
            seen.iter().any(|status| status == "success"),
            "a real pull must end in the `success` frame this module reads as completion: {seen:?}"
        );
    }

    // ---------------------------------------------------------------------------------------
    // K. What a model weighs, and whether this machine can carry it
    // ---------------------------------------------------------------------------------------

    /// A loopback stand-in for the public registry, answering one manifest.
    ///
    /// Registered under the full `/v2/{path}` shape rather than a wildcard so a mistake in
    /// `registry_path` shows up as a 404 here instead of passing silently — the path IS half of
    /// what this section is testing.
    async fn stub_registry(path: &'static str, body: &'static str) -> String {
        let app = axum::Router::new().route(
            &format!("/v2/{path}"),
            axum::routing::get(move || async move { body }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        format!("http://{address}")
    }

    /// The four name shapes Ollama takes, each to the one path the registry serves it at.
    ///
    /// `latest` and `library` are Ollama's own defaults, applied here rather than at the call site
    /// so every caller spells a bare `qwen3` the same way.
    #[test]
    fn every_shape_of_model_name_lands_on_the_path_the_registry_serves_it_at() {
        assert_eq!(
            registry_path("qwen3:8b").as_deref(),
            Some("library/qwen3/manifests/8b")
        );
        assert_eq!(
            registry_path("qwen3").as_deref(),
            Some("library/qwen3/manifests/latest"),
            "a name with no tag is `latest`, which is what `ollama pull` means by it"
        );
        assert_eq!(
            registry_path("hf.co/model:q4").as_deref(),
            Some("hf.co/model/manifests/q4")
        );
        assert_eq!(
            registry_path("someone/model").as_deref(),
            Some("someone/model/manifests/latest")
        );
    }

    /// The name reaches a URL path, so it is an allowlist and never an escape.
    ///
    /// This is the one field in this section that comes from outside — `http.rs` takes it from a
    /// request — and it is interpolated into a path on a host this crate names. A name that could
    /// carry `..` or its own query would let a caller aim the read somewhere else on that host, so
    /// the first byte must be alphanumeric and the rest a closed set. Rejecting is the whole
    /// answer: there is no sanitising branch that tries to rescue a name, because a name this does
    /// not recognise is not a model anyway.
    #[test]
    fn a_name_that_could_steer_the_path_is_refused_rather_than_cleaned() {
        for hostile in [
            "../../etc/passwd",
            "..",
            ".hidden",
            "qwen3:../../x",
            "a/b/c",
            "qwen3:8b?x=1",
            "qwen3:8b#f",
            "qwen 3",
            "qwen3:",
            ":8b",
            "",
            "qwen3:8b/../..",
            "%2e%2e/x",
        ] {
            assert_eq!(
                registry_path(hostile),
                None,
                "`{hostile}` must be refused, not repaired"
            );
        }
    }

    /// The manifest's own arithmetic: every layer counts, because every layer is downloaded.
    #[test]
    fn a_models_size_is_the_sum_of_every_layer_the_manifest_lists() {
        let manifest = r#"{
            "schemaVersion": 2,
            "config": {"size": 487},
            "layers": [
                {"mediaType": "application/vnd.ollama.image.model", "size": 4683074048},
                {"mediaType": "application/vnd.ollama.image.system", "size": 68},
                {"mediaType": "application/vnd.ollama.image.template", "size": 1615},
                {"mediaType": "application/vnd.ollama.image.license", "size": 11343}
            ]
        }"#;
        assert_eq!(
            interpret_manifest(manifest),
            Some(4_683_087_074),
            "the real qwen2.5-coder:7b manifest, measured against the live registry"
        );
    }

    /// Unreadable is `None` and never `Some(0)`.
    ///
    /// Zero is a real answer meaning "weighs nothing", and a caller grading a download against
    /// this machine's memory would read it as "fits comfortably" — the most permissive verdict
    /// there is, produced by the failure to say anything at all.
    #[test]
    fn a_manifest_this_crate_cannot_read_yields_no_size_rather_than_a_zero() {
        assert_eq!(interpret_manifest("not json"), None);
        assert_eq!(interpret_manifest("{}"), None, "no layers key at all");
        assert_eq!(interpret_manifest(r#"{"layers": []}"#), None, "no layers");
        assert_eq!(
            interpret_manifest(r#"{"layers": [{"mediaType": "x"}]}"#),
            None,
            "a layer with no size is a manifest this crate does not understand"
        );
    }

    #[tokio::test]
    async fn the_registry_answers_what_a_model_weighs_before_a_byte_of_it_is_downloaded() {
        let base = stub_registry(
            "library/qwen3/manifests/8b",
            r#"{"layers": [{"size": 5230000000}]}"#,
        )
        .await;
        assert_eq!(
            registry_model_size(&reqwest::Client::new(), &base, "qwen3:8b").await,
            Ok(5_230_000_000)
        );
    }

    /// A model the registry does not have comes back as a refusal a person can read.
    #[tokio::test]
    async fn a_name_the_registry_does_not_serve_is_an_error_and_not_a_guess() {
        let base = stub_registry("library/qwen3/manifests/8b", "{}").await;
        let outcome = registry_model_size(&reqwest::Client::new(), &base, "no-such-model:9b").await;
        assert!(outcome.is_err(), "a 404 must not read as a size");
        assert!(
            outcome.unwrap_err().contains("no-such-model:9b"),
            "the refusal has to name what was asked for"
        );
    }

    #[tokio::test]
    async fn a_name_the_path_builder_refuses_never_reaches_the_network() {
        // The base URL is deliberately one nothing is listening on: if this reached the network at
        // all the test would fail on the connection rather than on the name, which is the point.
        let outcome =
            registry_model_size(&reqwest::Client::new(), "http://127.0.0.1:1", "../x").await;
        assert!(matches!(outcome, Err(reason) if reason.contains("not a model name")));
    }

    /// The grading, which is the whole feature: three verdicts, and the middle one earns its place.
    ///
    /// Measured on the machine this was written for — 15.8 GB of RAM — against sizes read from the
    /// live registry, so these are not invented numbers: `qwen3:8b` is 5.23 GB and runs,
    /// `gemma3:27b` is 17.4 GB and does not, and the gap between them is where `Tight` lives.
    ///
    /// The headroom is a fifth of the weights plus a gigabyte: a model needs its weights resident
    /// PLUS a KV cache that grows with the context, and the machine needs to keep running. Without
    /// it a 15 GB model on a 16 GB machine reads as "fits", and what actually happens is the
    /// system swaps until somebody force-quits it.
    #[test]
    fn a_model_is_graded_against_what_this_machine_actually_has() {
        const GB: u64 = 1_000_000_000;
        let machine = 15_800 * GB / 1000;

        assert_eq!(
            model_fit(5_230_000_000, machine),
            Fit::Comfortable,
            "qwen3:8b, 5.23 GB, on 15.8 GB — this is the case the feature exists to say yes to"
        );
        assert_eq!(
            model_fit(17_400_000_000, machine),
            Fit::TooBig,
            "gemma3:27b, 17.4 GB, is larger than the whole machine before any headroom"
        );
        assert_eq!(
            model_fit(42_520_000_000, machine),
            Fit::TooBig,
            "llama3.3:70b, 42.5 GB"
        );
        assert_eq!(
            model_fit(11 * GB, machine),
            Fit::Tight,
            "11 GB needs 14.2 GB of a 15.8 GB machine: it runs, and saying so plainly is not the \
             same as recommending it"
        );
    }

    /// Zero memory is "cannot say", not "nothing fits".
    ///
    /// `total_memory_bytes` returns `None` off Windows and on any failure of the one call it
    /// makes, and a caller that turned that into `TooBig` would refuse every download on a machine
    /// this crate merely could not measure.
    #[test]
    fn a_machine_whose_memory_could_not_be_read_grades_nothing() {
        assert_eq!(model_fit(5_230_000_000, 0), Fit::Unknown);
        assert_eq!(
            model_fit(0, 16_000_000_000),
            Fit::Unknown,
            "a size of zero is the unreadable-manifest case and must not read as `fits`"
        );
    }

    /// Overflow is a wrong verdict, not a panic, and the wrong verdict is the permissive one.
    #[test]
    fn an_absurd_size_saturates_rather_than_wrapping_into_a_yes() {
        assert_eq!(model_fit(u64::MAX, 16_000_000_000), Fit::TooBig);
    }

    /// Against this machine, which is the only place the number is real.
    ///
    /// `#[ignore]`d not because it is slow but because it asserts a fact about the HOST: it passes
    /// on any Windows machine with more than a gigabyte and is meaningless on a CI runner with a
    /// different one. The three siblings in section F are ignored for the same class of reason.
    #[test]
    #[ignore = "asserts a fact about the machine it runs on, not about this crate"]
    fn this_machine_reports_a_memory_size_that_is_not_absurd() {
        let memory = total_memory_bytes().expect("Windows must answer GlobalMemoryStatusEx");
        assert!(
            memory > 1_000_000_000 && memory < 100_000_000_000_000,
            "a plausible amount of RAM, got {memory}"
        );
    }

    /// Off Windows too, because every machine has some physical memory.
    ///
    /// `#[cfg(unix)]` and deliberately NOT `#[ignore]`d, which is the whole difference from the
    /// Windows sibling above: that one pins a RANGE, which is a fact about one host, while this
    /// one pins only that an answer exists at all. Any machine that can run this suite has RAM,
    /// so a `None` here is the platform gap — `total_memory_bytes` is implemented against the
    /// Windows API alone — and never a property of the host the test ran on.
    #[cfg(unix)]
    #[test]
    fn a_unix_machine_reports_its_memory() {
        let memory = total_memory_bytes();
        assert!(
            matches!(memory, Some(bytes) if bytes > 0),
            "a Unix machine has physical memory and must report some of it, got {memory:?}"
        );
    }

    // ---------------------------------------------------------------------------------------
    // L. That this crate can speak HTTPS at all
    // ---------------------------------------------------------------------------------------
    //
    // Every other fixture in this file is an `axum` listener on `http://127.0.0.1:<port>`, and a
    // loopback stub never needs TLS. So 3339 tests passed over a crate whose `reqwest` was built
    // `default-features = false, features = ["json"]` — no `native-tls`, no `rustls`, nothing at
    // all in `Cargo.lock` — and which therefore could not complete a single `https://` request.
    //
    // It mattered to exactly the two constants that are not loopback: `openai_compatible::
    // OPENROUTER_BASE_URL` and `OLLAMA_REGISTRY_URL`. The hosted assistant route had never
    // successfully made a request in its life, and nobody noticed because using it needs a key
    // nobody on this machine had. Found by pointing the size route at the real registry and
    // reading the error instead of the verdict.
    //
    // The lesson is the shape of the cover rather than the missing feature: a stub that is easy to
    // write is a stub that answers a different question than production asks.

    /// The guard, and it is a COMPILE-time one on purpose.
    ///
    /// `Client::builder().build()` succeeds perfectly well with no TLS backend — the failure
    /// appears only on the first `https://` request, at runtime, inside whatever feature happened
    /// to need one. `use_native_tls` exists only while the feature does, so deleting it from
    /// `Cargo.toml` breaks the BUILD instead of one route months later. That is the whole value
    /// here; the assertion below is almost incidental to it.
    #[test]
    fn the_crate_is_built_with_a_tls_backend_and_stops_compiling_without_one() {
        let client = reqwest::Client::builder().use_native_tls().build();
        assert!(
            client.is_ok(),
            "a client with the native TLS backend must build: {client:?}"
        );
    }

    /// The registry, for real, over TLS. Keyless.
    ///
    /// `#[ignore]`d because it needs the internet, exactly as its siblings in section F need a
    /// running Ollama — and NOT because it is optional. This is the test whose absence let the
    /// crate ship unable to speak HTTPS, so it earns a run by hand after any change to the client
    /// builders or to `reqwest`'s feature list:
    ///
    ///   cargo test -p nucleos-core -- --ignored --nocapture capabilities::tests::the_real_registry
    ///
    /// The size is asserted as a RANGE rather than a number: `qwen3:8b` was 5.23 GB when this was
    /// written, and a re-quantised upload would move it — which is the registry doing its job, not
    /// this crate breaking.
    #[tokio::test]
    #[ignore = "needs the internet: reaches the real Ollama registry over TLS"]
    async fn the_real_registry_answers_over_tls_what_a_model_weighs() {
        let size = registry_model_size(&reqwest::Client::new(), OLLAMA_REGISTRY_URL, "qwen3:8b")
            .await
            .expect("the public registry must answer a manifest for a model it serves");

        assert!(
            (3_000_000_000..12_000_000_000).contains(&size),
            "qwen3:8b should weigh a few gigabytes, got {size}"
        );
    }

    /// The hosted route's own catalogue read, for real, over TLS. Also keyless.
    ///
    /// `discover_openai_compatible` takes no key — `GET {base}/models` is public — so the one part of the
    /// hosted route that can be exercised without an account is exercised here. That is the point:
    /// the route was unreachable for a reason that had nothing to do with credentials, and this is
    /// the cheapest test that would have said so.
    #[tokio::test]
    #[ignore = "needs the internet: reaches the real OpenRouter catalogue over TLS"]
    async fn the_real_openrouter_catalogue_is_readable_over_tls() {
        let declared = discover_openai_compatible(
            &reqwest::Client::new(),
            crate::openai_compatible::OPENROUTER_BASE_URL,
            "anthropic/claude-sonnet-4.5",
            8_000,
            None,
        )
        .await;

        assert_eq!(
            declared.context,
            Ok(()),
            "a model with a million-token window must satisfy an 8k requirement — an Err here is \
             the catalogue not having been read at all, which is exactly what a missing TLS \
             backend looks like from the outside"
        );
        assert!(
            declared.tools,
            "the live catalogue lists `tools` among this model's supported parameters"
        );
    }

    // ---------------------------------------------------------------------------------------
    // M. The probe follows the configured engine
    // ---------------------------------------------------------------------------------------

    /// A loopback stand-in for whatever local server the operator actually runs, answering BOTH
    /// capability dialects on ONE address: Ollama's `POST /api/show` and the OpenAI-compatible
    /// `GET /models`, each with its own hit counter.
    ///
    /// Co-hosted rather than bound on two listeners, for the reason `assistants.rs`'s
    /// `stub_both_local_catalogues` already records: the code under test is handed ONE base url,
    /// so a counter sitting on a second address nothing can name could never be incremented and
    /// would assert nothing, whatever the probe did with it. One address is also what production
    /// really looks like — an operator who swapped Ollama for llama.cpp on the same port.
    async fn stub_both_local_capability_routes(
        catalogue: serde_json::Value,
    ) -> (
        String,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
    ) {
        let show_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted_show = show_hits.clone();
        let models_hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counted_models = models_hits.clone();
        let app = axum::Router::new()
            .route(
                "/api/show",
                axum::routing::post(move || {
                    let counted_show = counted_show.clone();
                    async move {
                        counted_show.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        axum::Json(serde_json::json!({
                            "model_info": {
                                "general.architecture": "qwen2",
                                "qwen2.context_length": 32768
                            },
                            "capabilities": ["completion"]
                        }))
                    }
                }),
            )
            .route(
                "/models",
                axum::routing::get(move || {
                    let catalogue = catalogue.clone();
                    let counted_models = counted_models.clone();
                    async move {
                        counted_models.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        axum::Json(catalogue)
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{address}"), show_hits, models_hits)
    }

    /// Without this, `local_engine: openai_compatible` is a feature that is green in every test and dead in
    /// production, which is the worst of the two ways to be broken.
    ///
    /// `main.rs`'s startup decides `local_model` by probing `discover_ollama_as` against the
    /// hardcoded `runner::OLLAMA_BASE_URL` — the Ollama dialect, at Ollama's address — BEFORE it
    /// reaches the `local_engine()` block that would have told it neither is what the operator
    /// configured. On an `local_engine: openai_compatible` install that probe posts `/api/show` to a
    /// llama.cpp or LM Studio server that has no such route, `interpret_context_probe` fails
    /// closed on the 404 body exactly as this module's doc requires, `local_assistant_posture`
    /// answers `FellBackToCli`, and `local_model` becomes `None`. The route the factory was
    /// taught to serve is then never handed a model to serve it with: the operator is told the
    /// local assistant is disabled, and the server they pointed the daemon at sits idle. Nothing
    /// in the suite notices today because the suite tests the FACTORY and never that startup
    /// sequence, which is the gap this test closes.
    ///
    /// The zero on the `/api/show` counter is the half that carries the proof. A probe that asked
    /// BOTH servers would report the right answer and still be wrong — the extra request is a
    /// dialect this install may have deliberately stopped serving, and a `Declared` that is right
    /// by accident goes wrong the moment Ollama is uninstalled.
    ///
    /// On the `role` argument, since this test is what fixes the signature: it is kept on both
    /// arms and is inert on the OpenAiCompatible one. `discover_ollama_as` interpolates it into the
    /// `could not read the {probe} probe response` wording an operator actually reads, while
    /// `discover_openai_compatible` names no probe at all — its failures ride in `Declared.context` and are
    /// worded once, by `missing_capabilities`. Kept anyway so the three `main.rs` call sites keep
    /// ONE signature to call rather than choosing one per engine, which is the shape that let the
    /// hardcoded dialect survive in the first place.
    #[tokio::test]
    async fn a_local_assistant_probe_follows_the_configured_engine_instead_of_assuming_ollama() {
        let (base_url, show_hits, models_hits) =
            stub_both_local_capability_routes(serde_json::json!({
                "data": [{
                    "id": "qwen3-coder-30b",
                    "context_length": 32768,
                    "supported_parameters": ["tools"],
                    "architecture": {"input_modalities": ["text"]}
                }]
            }))
            .await;

        let declared = discover_local_as(
            &reqwest::Client::new(),
            crate::config::LocalEngine::OpenAiCompatible,
            &base_url,
            "qwen3-coder-30b",
            8192,
            None,
            "local assistant",
        )
        .await;

        assert_eq!(
            models_hits.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "an `openai_compatible` engine must discover over `GET /models`, which this stub counts"
        );
        assert_eq!(
            declared.context,
            Ok(()),
            "the catalogue states a 32768-token window, which must satisfy 8192 — an `Err` here \
             is the probe having read nothing usable, which is what `local_model: None` is made \
             of: {declared:?}"
        );
        assert_eq!(
            show_hits.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "an `openai_compatible` engine must never post Ollama's `/api/show`: that request is the \
             hardcoded dialect this test exists to remove, and on a machine without Ollama it is \
             the request that disables the local assistant"
        );
    }
}
