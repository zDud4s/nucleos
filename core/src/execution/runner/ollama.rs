//! The Ollama backend: one-shot runs and chats against the loopback API.

use super::*;

/// A tool-free Ollama boundary for local triage.
///
/// This is deliberately a separate `CommandRunner` rather than a mode on `ClaudeCliRunner`: the
/// chat endpoint has no tool or session protocol, and pretending otherwise would make ignored CLI
/// controls look enforced. The retained client also reuses connections across local batches.
pub struct OllamaRunner {
    pub(super) client: reqwest::Client,
    pub(super) base_url: String,
    pub(super) model: String,
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
pub(super) fn unusable_local_answer(answer: &str) -> Option<String> {
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
    pub(super) client: reqwest::Client,
    pub(super) base_url: String,
    pub(super) model: String,
}

/// Ceiling on one exchange with the local model.
///
/// A turn is up to `MAX_TOOL_ROUNDS` of these, so this is per round rather than per turn — the turn
/// itself is bounded again by the caller. Generous because a cold model loads from disk on the
/// first request, and a first message that times out while Ollama is still starting looks exactly
/// like a broken bot.
pub const OLLAMA_EXCHANGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

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
impl crate::models::local_agent::LocalChat for OllamaChat {
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
            serde_json::json!({"num_ctx": crate::models::local_agent::TURN_NUM_CTX}),
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
        if message_count > crate::governance::triage::LOCAL_BATCH_MAX {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!(
                    "local triage prompt contains {message_count} messages; at most {} fit the probed context contract",
                    crate::governance::triage::LOCAL_BATCH_MAX
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
                "num_ctx": crate::governance::triage::LOCAL_NUM_CTX,
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
