//! The núcleo↔**OpenAI-compatible chat** boundary.
//!
//! `OpenAiCompatibleChat` is a second `crate::local_agent::LocalChat` implementation, beside
//! `runner::OllamaChat`, so the same tool loop the local model uses can instead be pointed at any
//! endpoint speaking OpenAI's `/chat/completions` — unlike Ollama's own `/api/chat` body. The
//! dialect is what this module knows; the vendor is not. OpenRouter is one user of it, reached
//! over TLS with a key, and a server answering the same route from somewhere else is another: both
//! send the same request and both answer with `choices[0].message`, so both are served here rather
//! than by a second client per provider. The loop itself (`local_agent::run_turn`) does not know
//! or care which of the two shapes it is talking to; this module's whole job is making the second
//! one answer in the shape the first one already does, so nothing upstream of `LocalChat` has to
//! learn a second dialect.
//!
//! Nothing else in the crate should ever build a `/chat/completions` request directly — a second
//! copy of the base URL, the auth header or the response-parsing rule is the thing that drifts
//! first, exactly as `runner.rs`'s doc argues for `ollama_chat` being the one HTTP client pointed
//! at Ollama. What is true of OpenRouter alone rather than of the wire keeps its name and says so:
//! `OPENROUTER_BASE_URL` is that provider's address and `OPENROUTER_EXCHANGE_TIMEOUT` is sized for
//! its routing, and neither is a fact about `/chat/completions`.

use async_trait::async_trait;
use serde_json::Value;

/// OpenRouter's API base. `/chat/completions` is appended by whichever call needs it, the same
/// split `runner::OllamaChat`'s `base_url` field makes rather than baking the whole URL into a
/// single constant: a test pointing this at a stub server instead of the real endpoint needs
/// somewhere to substitute, and a `&'static str` with the path already on it would leave nothing to
/// substitute.
pub const OPENROUTER_BASE_URL: &str = "https://openrouter.ai/api/v1";

/// An OpenAI-compatible `/chat/completions` client, as the one exchange `local_agent`'s loop needs
/// — `runner::OllamaChat`'s sibling and not a variant of it, because the two disagree about more
/// than a URL: Ollama answers its own `/api/chat` dialect, and this one answers OpenAI's.
/// `base_url` is a field rather than a constant for the same reason the type is named for the wire
/// and not for a provider: OpenRouter is where it points today, and pointing it elsewhere is meant
/// to cost a different string and nothing else.
///
/// `key` is an `Option` because the dialect is not the credential, and the two travel apart. A
/// hosted endpoint must be told who is asking; a server answering this same route from loopback —
/// `llama.cpp`, vLLM, LM Studio — asks for nobody, and sending it a key anyway puts a secret on a
/// wire that never wanted one. `secrets.rs` states the rule this obeys: the key is read from the OS
/// Credential Manager for the endpoint that needs it, and an `Authorization` header is a secret
/// leaving the process whether or not the receiver ever reads it. `None` is that absence said
/// outright, told apart from a caller inventing an empty string to mean the same thing.
pub struct OpenAiCompatibleChat {
    client: reqwest::Client,
    base_url: String,
    model: String,
    key: Option<String>,
}

/// Ceiling on one exchange with the hosted model.
///
/// A turn is up to `MAX_TOOL_ROUNDS` of these, so this is per round rather than per turn — the turn
/// itself is bounded again by the caller, the same split `runner::OLLAMA_EXCHANGE_TIMEOUT` makes.
/// Generous for a different reason than the loopback client's: there is no cold model to load off
/// local disk here, but OpenRouter's own routing can retry a request across more than one upstream
/// provider before answering, and a request that times out mid-route reads exactly like a hung
/// server rather than what it actually is.
pub const OPENROUTER_EXCHANGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

impl OpenAiCompatibleChat {
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
    ///
    /// That refusal STAYS, now that the struct's own `key` is an `Option<String>`: this is the
    /// HOSTED constructor, and its refusal is `assistants::Refusal::HostedModelNamedButNoKey`'s
    /// sibling — the same sentence said at the two places a hosted chat can be built from. A
    /// keyless server is reached by handing `with_client` a `None` outright, deliberately, and
    /// never by calling this and letting it decide that no key was fine after all; so "no key
    /// reached `new`" still means a hosted chat was asked for with nothing to authenticate it, and
    /// still refuses before a request leaves the machine.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn new(base_url: String, model: String, key: Option<String>) -> std::io::Result<Self> {
        let key = key.ok_or_else(|| {
            std::io::Error::other(
                "no OpenRouter key is stored; add one before starting a hosted chat",
            )
        })?;
        Ok(Self {
            // A client timeout, not a default client, for the same reason
            // `assistants::ConfiguredAssistants::new` gives at its `local_client`:
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
            key: Some(key),
        })
    }

    /// Builds from a client the caller already owns, rather than one built fresh here — what
    /// `assistants::ConfiguredAssistants` needs so every `OpenAiCompatibleChat` it hands out shares the
    /// ONE `reqwest::Client` the hosted route was constructed with. `key` arrives as
    /// `Option<String>` and is taken at its word both ways. `Some` is the hosted route: the refusal
    /// `new`'s `key.ok_or_else` performs above has moved up into
    /// `assistants::Refusal::HostedModelNamedButNoKey`, read once by the caller before this runs,
    /// so by the time a `ConfiguredAssistants` reaches for this constructor a key is already in
    /// hand. `None` is a local OpenAI-compatible server that asks for no key — the case `new`
    /// cannot express, because `new` refuses it. `new` above stays as it is for every other caller.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn with_client(
        client: reqwest::Client,
        base_url: String,
        model: String,
        key: Option<String>,
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
impl crate::local_agent::LocalChat for OpenAiCompatibleChat {
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
        let mut request = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .json(&body);
        // The header is attached only when there is a key, rather than unconditionally with an
        // empty string standing in for none. A loopback server that needs no key must not be sent
        // one: an `Authorization` header is a secret leaving the process, and `secrets.rs`'s rule
        // is that the key is read for the endpoint that needs it — a receiver that ignores what it
        // never asked for does not make the send safe, because what is on the wire is not the
        // receiver's decision. The hosted route is untouched by this: `Some` still bears exactly
        // the `Bearer <key>` it always did.
        if let Some(key) = &self.key {
            request = request.bearer_auth(key);
        }

        let response = request
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
    use crate::local_agent::LocalChat;

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
        let result = OpenAiCompatibleChat::new(
            OPENROUTER_BASE_URL.to_string(),
            "anthropic/claude-sonnet-4.5".to_string(),
            None,
        );

        assert!(
            result.is_err(),
            "new() built a client with no key instead of refusing outright"
        );
    }

    /// A loopback `/chat/completions` that KEEPS the HEADERS it was sent, answering the one shape
    /// `assistant_message` above reads. Copied from `assistants::tests::stub_completions_recording`
    /// rather than imported, because that one lives inside a private `#[cfg(test)] mod tests` and
    /// records BODIES — the fact under test there is what was asked, and the fact under test here
    /// is who the asking claimed to be. `127.0.0.1:0` so the OS picks the port: nothing in this
    /// suite may reach a real endpoint, and a fixed port is a collision with whatever else is bound
    /// when the whole gate runs at once.
    async fn stub_completions_recording_headers() -> (
        String,
        std::sync::Arc<std::sync::Mutex<Vec<axum::http::HeaderMap>>>,
    ) {
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = seen.clone();
        let app = axum::Router::new().route(
            "/chat/completions",
            axum::routing::post(
                move |headers: axum::http::HeaderMap,
                      axum::Json(_body): axum::Json<serde_json::Value>| {
                    let recorded = recorded.clone();
                    async move {
                        recorded
                            .lock()
                            .expect("the stub's recorder is never held across an await")
                            .push(headers);
                        axum::Json(serde_json::json!({
                            "choices": [{ "message": { "content": "ok" } }]
                        }))
                    }
                },
            ),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{address}"), seen)
    }

    /// A server that asks for no key must not be sent one.
    ///
    /// `with_client` took `key: String`, so every request this module built carried
    /// `.bearer_auth(..)` unconditionally — correct for OpenRouter, which requires it, and a secret
    /// leaving the process for a loopback `/chat/completions` that never asked. That is the rule
    /// `secrets.rs` states: the key is read from the OS Credential Manager for the endpoint that
    /// needs it, and an `Authorization` header on a wire that does not is a leak whether or not the
    /// receiver ever reads it. Nothing else can witness this — a response does not carry the
    /// request's own headers back, so without a stub that records them the header goes out and no
    /// part of the tree says so.
    #[tokio::test]
    async fn a_keyless_local_server_is_asked_with_no_authorization_header() {
        let (base_url, seen) = stub_completions_recording_headers().await;

        let chat = OpenAiCompatibleChat::with_client(
            reqwest::Client::new(),
            base_url,
            "qwen3:8b".to_string(),
            None,
        );
        chat.exchange(
            vec![serde_json::json!({ "role": "user", "content": "hello" })],
            None,
        )
        .await
        .expect("the stub answers one usable choice");

        let seen = seen
            .lock()
            .expect("the stub's recorder is never held across an await");
        assert_eq!(
            seen.len(),
            1,
            "the keyless route made {} requests, not one",
            seen.len()
        );
        assert!(
            seen[0].get(axum::http::header::AUTHORIZATION).is_none(),
            "a key-less client still sent an Authorization header: {:?}",
            seen[0].get(axum::http::header::AUTHORIZATION)
        );
    }

    /// The other half of the same change: making the key OPTIONAL must not quietly unauthenticate
    /// the hosted route.
    ///
    /// A `None`-tolerant `bearer_auth` written on the wrong side of its own condition drops the
    /// header for every caller, and OpenRouter answers that with a 401 — which reaches the person
    /// in the chat as "the model did not answer", the exact confusion `classify` and
    /// `assistant_message` exist to prevent one layer over. An `is_err()` assertion would not catch
    /// it either, since a stub answers anything put to it; only the header that actually arrived
    /// can.
    #[tokio::test]
    async fn the_hosted_route_still_carries_its_bearer_key() {
        let (base_url, seen) = stub_completions_recording_headers().await;

        let chat = OpenAiCompatibleChat::with_client(
            reqwest::Client::new(),
            base_url,
            "anthropic/claude-sonnet-4.5".to_string(),
            Some("sk-or-v1-test".to_string()),
        );
        chat.exchange(
            vec![serde_json::json!({ "role": "user", "content": "hello" })],
            None,
        )
        .await
        .expect("the stub answers one usable choice");

        let seen = seen
            .lock()
            .expect("the stub's recorder is never held across an await");
        assert_eq!(
            seen.len(),
            1,
            "the hosted route made {} requests, not one",
            seen.len()
        );
        assert_eq!(
            seen[0]
                .get(axum::http::header::AUTHORIZATION)
                .and_then(|value| value.to_str().ok()),
            Some("Bearer sk-or-v1-test"),
            "the hosted route lost its bearer key when the key became optional"
        );
    }

    /// The measured number always beats the stated one.
    ///
    /// `resolve_context_window` exists because an OpenAI-compatible server may or may not publish
    /// `context_length` on `/models`, so a window declared in configuration is a FALLBACK and never
    /// an override. Let a declared 8192 replace a probed 32768 and configuration silently truncates
    /// a server that told the truth — `voice.rs`'s lesson re-expressed on a wire that is not
    /// Ollama's: a prompt that does not fit is cut, nobody is told, and the turn that forgets its
    /// first lookup reads as a bad model rather than a bad number.
    #[test]
    fn a_probed_context_length_is_discovered_and_never_replaced_by_a_declared_one() {
        let catalogue = serde_json::json!({
            "data": [{
                "id": "qwen/qwen3-8b",
                "context_length": 32768,
                "supported_parameters": ["tools"]
            }]
        })
        .to_string();

        let window = crate::capabilities::resolve_context_window(
            &catalogue,
            "qwen/qwen3-8b",
            Some(8192),
            crate::local_agent::TURN_NUM_CTX,
        );

        assert_eq!(
            window,
            Ok(crate::capabilities::ContextWindow::Discovered(32768)),
            "a declared window overrode the one the server itself published"
        );
    }

    /// The fallback, and the fact that it says it is one.
    ///
    /// A catalogue entry carrying no `context_length` is what a minimal OpenAI-compatible server
    /// answers, and discovery alone calls that `MissingContextLength` and refuses. Refusing is
    /// right when nothing is known; here the operator has stated the window, so the stated number
    /// is the answer — carried as `Declared` and not as `Discovered`, because the two are different
    /// claims. A caller that cannot tell a measured window from a stated one reports somebody's
    /// configuration as a fact about the server.
    #[test]
    fn a_catalogue_with_no_context_length_falls_back_to_the_declared_window_and_says_so() {
        let catalogue = serde_json::json!({
            "data": [{
                "id": "qwen/qwen3-8b",
                "supported_parameters": ["tools"]
            }]
        })
        .to_string();

        let window = crate::capabilities::resolve_context_window(
            &catalogue,
            "qwen/qwen3-8b",
            Some(65536),
            crate::local_agent::TURN_NUM_CTX,
        );

        assert_eq!(
            window,
            Ok(crate::capabilities::ContextWindow::Declared(65536)),
            "a window nobody probed came back as if the server had published it"
        );
    }

    /// Neither probed nor declared is refused, never guessed.
    ///
    /// This is `local_agent::TURN_NUM_CTX`'s guard re-expressed on a wire that has no `num_ctx`
    /// field to state it on. A turn ACCUMULATES — system message, question, then every tool schema
    /// and every tool result again on each round — so a window assumed from a vendor default is how
    /// a long conversation becomes a short one nobody was told about. Defaulting to any number here
    /// would make this function always succeed and move the failure to the first truncated answer,
    /// by which point nothing attributes it to the window.
    #[test]
    fn a_window_neither_probed_nor_declared_is_refused_with_the_missing_context_length() {
        let catalogue = serde_json::json!({
            "data": [{
                "id": "qwen/qwen3-8b",
                "supported_parameters": ["tools"]
            }]
        })
        .to_string();

        let window = crate::capabilities::resolve_context_window(
            &catalogue,
            "qwen/qwen3-8b",
            None,
            crate::local_agent::TURN_NUM_CTX,
        );

        assert_eq!(
            window,
            Err(crate::runner::ModelError::MissingContextLength),
            "a window neither probed nor declared was answered with a guess"
        );
    }
}
