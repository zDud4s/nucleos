//! What a role needs from a model, what a model declares, and what each role does about the gap.
//!
//! Exactly one capability is verified today: the context window, read three times over in
//! `main.rs` (local triage, voice cleanup, the local assistant) through the same `/api/show` probe
//! and the same `runner::interpret_context_probe`. This module replaces the three copies with one
//! uniform check that produces exactly the postures those three sites already implement by hand —
//! it changes no behaviour, it only makes the behaviour that already exists verifiable in code.
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
/// Scoped to the non-test build, for the same reason `discover_openrouter` below is: every
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

/// OpenRouter route: `GET {base_url}/models`, the same catalogue `openrouter.rs`'s doc calls "the
/// hosted-chat endpoint's sibling" — OpenRouter states each model's window and supported parameters
/// in its own catalogue entry rather than answering a per-model probe the way Ollama's `/api/show`
/// does, so there is no request here for `interpret_context_probe` to read; the comparison against
/// `required_tokens` is this function's own, against the number the catalogue states outright.
///
/// Its first production caller is `assistants.rs`'s `ConfiguredAssistants::declared_one`, for
/// `Brain::OpenRouter` — the `#[cfg_attr(not(test), allow(dead_code))]` this doc used to carry is
/// gone, per its own instruction to delete it "the day the model picker calls it": that day is
/// this one, and a caller that stops reaching this function is a regression clippy should now
/// catch, not one this attribute should keep hiding.
pub async fn discover_openrouter(
    client: &reqwest::Client,
    base_url: &str,
    model: &str,
    required_tokens: usize,
) -> Declared {
    let body = match client.get(format!("{base_url}/models")).send().await {
        Ok(response) => match response.text().await {
            Ok(body) => body,
            Err(error) => {
                return undeclared(runner::ModelError::UnparseableResponse(format!(
                    "could not read the OpenRouter catalogue response: {error}"
                )));
            }
        },
        Err(error) => {
            return undeclared(runner::ModelError::UnparseableResponse(format!(
                "could not reach the OpenRouter catalogue: {error}"
            )));
        }
    };

    let parsed: serde_json::Value = match serde_json::from_str(&body) {
        Ok(value) => value,
        Err(error) => {
            return undeclared(runner::ModelError::UnparseableResponse(error.to_string()));
        }
    };

    let entry = parsed
        .get("data")
        .and_then(serde_json::Value::as_array)
        .and_then(|entries| {
            entries
                .iter()
                .find(|entry| entry.get("id").and_then(serde_json::Value::as_str) == Some(model))
        });
    let Some(entry) = entry else {
        return undeclared(runner::ModelError::ModelUnavailable(format!(
            "\"{model}\" is not in OpenRouter's catalogue"
        )));
    };

    let context = match entry.get("context_length") {
        None => Err(runner::ModelError::MissingContextLength),
        Some(value) => match value
            .as_u64()
            .and_then(|tokens| usize::try_from(tokens).ok())
        {
            None => Err(runner::ModelError::InvalidContextLength),
            Some(available_tokens) if available_tokens < required_tokens => {
                Err(runner::ModelError::ContextTooSmall {
                    available_tokens,
                    required_tokens,
                })
            }
            Some(_) => Ok(()),
        },
    };

    let supported_parameters = string_array(entry, "supported_parameters");
    let input_modalities = entry
        .get("architecture")
        .map(|architecture| string_array(architecture, "input_modalities"))
        .unwrap_or_default();

    Declared {
        context,
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
/// Scoped to the non-test build, for the same reason `discover_openrouter` above is: no production
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
/// Scoped to the non-test build, for the same reason `discover_openrouter` above is: every role
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

        let declared =
            discover_openrouter(&client, &base_url, "anthropic/claude-sonnet-4.5", 8192).await;

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
}
