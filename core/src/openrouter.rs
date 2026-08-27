//! The núcleo↔**OpenRouter** boundary.
//!
//! `OpenRouterChat` is a second `crate::local_agent::LocalChat` implementation, beside
//! `runner::OllamaChat`, so the same tool loop the local model uses can instead be pointed at a
//! HOSTED model over OpenRouter's `/chat/completions` — an OpenAI-shaped endpoint, unlike Ollama's
//! own `/api/chat` body. The loop itself (`local_agent::run_turn`) does not know or care which of
//! the two it is talking to; this module's whole job is making the second one answer in the shape
//! the first one already does, so nothing upstream of `LocalChat` has to learn a second dialect.
//!
//! Nothing else in the crate should ever build a request against OpenRouter directly — a second
//! copy of the base URL, the auth header or the response-parsing rule is the thing that drifts
//! first, exactly as `runner.rs`'s doc argues for `ollama_chat` being the one HTTP client pointed
//! at Ollama.

use async_trait::async_trait;
use serde_json::Value;

/// OpenRouter's API base. `/chat/completions` is appended by whichever call needs it, the same
/// split `runner::OllamaChat`'s `base_url` field makes rather than baking the whole URL into a
/// single constant: a test pointing this at a stub server instead of the real endpoint needs
/// somewhere to substitute, and a `&'static str` with the path already on it would leave nothing to
/// substitute.
pub const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// The hosted-chat endpoint, as the one exchange `local_agent`'s loop needs — `runner::OllamaChat`'s
/// sibling and not a variant of it, because the two disagree about more than a URL: Ollama is a
/// loopback service this machine trusts by construction, OpenRouter is somebody else's server that
/// must be told who is asking, so this carries a `key` where `OllamaChat` carries none.
pub struct OpenRouterChat {
    client: reqwest::Client,
    base_url: String,
    model: String,
    key: String,
}

/// Ceiling on one exchange with the hosted model.
///
/// A turn is up to `MAX_TOOL_ROUNDS` of these, so this is per round rather than per turn — the turn
/// itself is bounded again by the caller, the same split `runner::OLLAMA_EXCHANGE_TIMEOUT` makes.
/// Generous for a different reason than the loopback client's: there is no cold model to load off
/// local disk here, but OpenRouter's own routing can retry a request across more than one upstream
/// provider before answering, and a request that times out mid-route reads exactly like a hung
/// server rather than what it actually is.
pub(crate) const OPENROUTER_EXCHANGE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(120);

impl OpenRouterChat {
    /// Builds the client, or refuses.
    ///
    /// `key` arrives as `Option<String>` because the caller has already done the one read that
    /// touches the OS Credential Manager (`secrets::load_secret`, per `secrets.rs`'s doc — the key
    /// comes from there and never from a config file) and `None` is how "nothing is stored" is
    /// told apart from a caller inventing an empty string to mean the same thing. This function
    /// itself never calls `secrets::load_secret`: the keyring has no fake behind it, the same
    /// reason `github::spawn_gh` takes its token as a plain argument rather than reading the store
    /// itself — passed in, the refusal below is exercisable by a test with no Credential Manager
    /// anywhere near it.
    ///
    /// Refusing HERE, instead of building the client anyway and letting the first request come
    /// back 401, is the whole point: a 401 arrives after the request has already left the machine,
    /// and to a caller that only sees an `Err` it reads exactly like a network fault — indistin-
    /// guishable from the endpoint being unreachable, which is the same confusion
    /// `runner::ollama_chat`'s `classify` exists to prevent one layer over. A missing key is
    /// knowable before anything is sent, so the message a caller gets back names the actual
    /// problem instead of a symptom two hops downstream of it.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(base_url: String, model: String, key: Option<String>) -> std::io::Result<Self> {
        let key = key.ok_or_else(|| {
            std::io::Error::other(
                "no OpenRouter key is stored; add one before starting a hosted chat",
            )
        })?;
        Ok(Self {
            // A client timeout, not a default client, for the same reason `OllamaChat::new` gives:
            // `reqwest::Client::new()` waits for ever, and for ever here means the chat slot is
            // never released and every later message in that chat is refused until the caller
            // restarts it. `expect` rather than a fallback, because `Client::default()` is
            // `Client::new()` and would panic on the exact same TLS/proxy misconfiguration one
            // line later, just with a message that names nothing.
            client: reqwest::Client::builder()
                .timeout(OPENROUTER_EXCHANGE_TIMEOUT)
                .build()
                .expect("HTTP client for the hosted model (check TLS and proxy environment)"),
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
            key,
        })
    }

    /// Builds from a client the caller already owns, rather than one built fresh here — what
    /// `assistants::ConfiguredAssistants` needs so every `OpenRouterChat` it hands out shares the
    /// ONE `reqwest::Client` the hosted route was constructed with. `key` arrives as `String`, not
    /// `Option<String>`: the refusal `new`'s `key.ok_or_else` performs above has moved up into
    /// `assistants::Refusal::HostedModelNamedButNoKey`, read once by the caller before this runs,
    /// so by the time a `ConfiguredAssistants` reaches for this constructor a key is guaranteed to
    /// already be in hand. `new` above stays as it is for every other caller.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_client(
        client: reqwest::Client,
        base_url: String,
        model: String,
        key: String,
    ) -> Self {
        Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
            key,
        }
    }
}

/// PURE: turns an OpenRouter `/chat/completions` response body into the assistant message
/// `local_agent`'s loop consumes — the same shape the Ollama path already produces (`runner.rs`'s
/// `ollama_message` unwraps `message` off its own response the same way), an object that may carry
/// `content` and/or `tool_calls`. Matching that shape, rather than inventing a second one, is what
/// lets `local_agent::tool_calls` read either provider's answer with no second copy of its own
/// normalisation — arguments arriving as a JSON STRING are exactly what an OpenAI-shaped `tool_calls`
/// entry sends, which is the case that function already exists to handle.
///
/// OpenRouter and Ollama fail differently, too: Ollama answers a non-2xx status with a body
/// `ollama_message` reads directly (`runner.rs` line ~1808), while OpenRouter can answer 200 with an
/// `error` object and no usable `choices` at all — a model unavailable on this key, a content
/// filter, a routing miss with every upstream declining. Reading only the HTTP status would mistake
/// that 200 for success and hand the loop a message built from nothing; this reads the BODY instead,
/// so the `Err` it returns carries OpenRouter's own stated reason rather than a generic
/// "no choices" that tells nobody what to fix.
fn assistant_message(response: &Value) -> std::io::Result<Value> {
    if let Some(message) = response
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|choices| choices.first())
        .and_then(|choice| choice.get("message"))
    {
        return Ok(message.clone());
    }

    // No usable choice: read OpenRouter's own `error.message` rather than manufacturing a generic
    // one, because the two words that follow are the whole reason this function exists — "the
    // model did not answer" tells the person in the chat nothing they can act on, while "Insuffi-
    // cient credits" or "rate-limited" tells them exactly what to do next.
    let reason = response
        .get("error")
        .and_then(|error| error.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("OpenRouter returned no usable choices");

    Err(std::io::Error::other(format!("OpenRouter: {reason}")))
}

#[async_trait]
impl crate::local_agent::LocalChat for OpenRouterChat {
    async fn exchange(
        &self,
        messages: Vec<Value>,
        tools: Option<Vec<Value>>,
    ) -> std::io::Result<Value> {
        let mut body = serde_json::json!({
            "model": self.model,
            "messages": messages,
        });
        // `tools` is omitted entirely rather than sent as `null` when there are none: a model
        // handed a null `tools` key can still answer with a tool call nobody in this turn can
        // execute, the same reasoning `runner::ollama_message` states for its own `tools` field.
        if let Some(tools) = tools {
            body.as_object_mut()
                .expect("body is constructed as an object literal above")
                .insert("tools".to_string(), Value::from(tools));
        }

        // Same split as `runner::ollama_message`'s `classify`: a timeout is about this one request
        // and the next may well succeed, while every other failure means the endpoint itself is
        // unreachable and every request after it will fail the same way. The client's timeout
        // covers the body read as well as the request, so this is applied to both awaits below.
        fn classify(error: reqwest::Error) -> std::io::Error {
            if error.is_timeout() {
                std::io::Error::new(std::io::ErrorKind::TimedOut, error)
            } else {
                std::io::Error::other(error)
            }
        }

        // The BODY, not the status, decides success — read straight into the PURE reader rather
        // than branching on `status.is_success()` first, because OpenRouter's own failure shape
        // (`{"error": {"message", "code"}}`) is what `assistant_message` already knows how to turn
        // into a reason a person can act on, whether it arrives on a 200 with no usable choice or
        // on a 402/429 that carries the same object. A status-first branch here would just rebuild
        // a second, worse copy of that same reading one layer up.
        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.key)
            .json(&body)
            .send()
            .await
            .map_err(classify)?
            .json::<Value>()
            .await
            .map_err(classify)?;

        assistant_message(&response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // `choices[0].message` is where the reader must look, and this response carries BOTH `content`
    // and `tool_calls` on that one message — the same object `local_agent::tool_calls` and
    // `local_agent::content_of` are each free to read a different half of, so a reader that copied
    // only one field across would pass a narrower test than this and still break the loop.
    #[test]
    fn the_response_reader_maps_choices_zero_message_to_the_message_the_loop_consumes() {
        let response = serde_json::json!({
            "id": "gen-abc123",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": "The gate is green.",
                    "tool_calls": [{
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "run_gate",
                            "arguments": "{}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }]
        });

        let message =
            assistant_message(&response).expect("a response with a usable choice must parse");

        assert_eq!(
            message,
            serde_json::json!({
                "role": "assistant",
                "content": "The gate is green.",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "run_gate",
                        "arguments": "{}"
                    }
                }]
            }),
            "the reader must hand back choices[0].message untouched, not a repackaged copy of it"
        );
    }

    // OpenRouter answers a routing failure with HTTP 200 and an `error` object rather than a
    // non-2xx status — a model unavailable on this key, most often — so a caller reading only the
    // status code would call this success. The reason this test insists on the provider's OWN
    // words, and not merely on `is_err()`, is `runner::ollama_message`'s own doc one function over:
    // "the BODY, not just the status... a configuration mistake is indistinguishable from a network
    // one" without it. A generic "no usable choices" would repeat exactly that mistake here.
    #[test]
    fn a_response_with_no_usable_choices_carries_the_providers_own_reason_in_the_error() {
        let response = serde_json::json!({
            "error": {
                "message": "anthropic/claude-opus-5 is not available on this key",
                "code": 404
            }
        });

        let error = assistant_message(&response)
            .expect_err("a response with no choices must not parse into a message");

        assert!(
            error
                .to_string()
                .contains("anthropic/claude-opus-5 is not available on this key"),
            "the error dropped OpenRouter's own reason and said only: {error}"
        );
    }

    // Mirrors `github::spawn_gh`'s reason for taking its token as a plain argument: the Credential
    // Manager has no fake, so the only way to exercise "no key" hermetically is to pass `None`
    // straight in rather than emptying the real OS store around the test. What this pins is the
    // REFUSAL itself — a client built with an empty key would not fail until its first request came
    // back 401, by which point the caller has already spent a round trip discovering what `new`
    // could have said immediately.
    #[test]
    fn construction_without_a_key_is_refused_rather_than_producing_a_client_that_will_401_later() {
        let result = OpenRouterChat::new(
            OPENROUTER_BASE_URL.to_string(),
            "anthropic/claude-sonnet-4.5".to_string(),
            None,
        );

        assert!(
            result.is_err(),
            "new() built a client with no key instead of refusing outright"
        );
    }
}
