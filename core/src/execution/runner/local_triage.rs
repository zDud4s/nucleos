//! Whether a local Ollama model can hold the triage prompt: the context probe and its errors.

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
