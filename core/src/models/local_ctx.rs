//! The one `options.num_ctx` every local Ollama generative call sends, so Ollama never reloads
//! the model because two callers asked for different windows.

use std::borrow::Cow;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::execution::runner::ModelError;

/// The shared window when the operator declares none.
pub const DEFAULT_LOCAL_NUM_CTX: usize = 8192;
/// The smallest window a local model must offer; below it the model is refused.
pub const MIN_LOCAL_NUM_CTX: usize = 4096;

/// PURE: the value of `local_context_tokens` that sets the shared Ollama `num_ctx`: only when the
/// local engine is Ollama (key absent or `ollama`). With any other engine the key keeps its old
/// meaning, that server's window, and the shared value stays at its default.
pub fn declared_ollama_num_ctx(local_engine: Option<&str>, tokens: Option<usize>) -> Option<usize> {
    match local_engine {
        None | Some("ollama") => tokens,
        Some(_) => None,
    }
}

/// PURE: the distinct model names of `named`, absent ones dropped, first-seen order kept.
pub fn distinct_models<'a>(named: &[Option<&'a str>]) -> Vec<&'a str> {
    let mut out: Vec<&str> = Vec::new();
    for model in named.iter().flatten() {
        if !out.contains(model) {
            out.push(model);
        }
    }
    out
}

/// Marker appended when a text is cut to fit the window.
const TRUNCATION_MARKER: &str = "\n[truncated to fit the local model's context]";

static LOCAL_NUM_CTX: AtomicUsize = AtomicUsize::new(DEFAULT_LOCAL_NUM_CTX);

/// PURE: the declared value (or the default), capped by the probed model window.
pub fn resolve_local_num_ctx(declared: Option<usize>, probed_window: Option<usize>) -> usize {
    let wanted = declared.unwrap_or(DEFAULT_LOCAL_NUM_CTX);
    match probed_window {
        Some(window) => wanted.min(window),
        None => wanted,
    }
}

/// The process-wide `num_ctx` for local Ollama calls.
pub fn local_num_ctx() -> usize {
    LOCAL_NUM_CTX.load(Ordering::Relaxed)
}

/// Startup: stores the declared (or default) value and returns it.
pub fn init_local_num_ctx(declared: Option<usize>) -> usize {
    let value = resolve_local_num_ctx(declared, None);
    LOCAL_NUM_CTX.store(value, Ordering::Relaxed);
    value
}

/// PURE: a probe that found a window between the floor and `current` is accepted and shrinks the
/// value; below the floor the error stands. Returns the value to use.
pub fn clamp_probe_result(current: usize, context: &mut Result<(), ModelError>) -> usize {
    if let Err(ModelError::ContextTooSmall {
        available_tokens, ..
    }) = context
    {
        let available = *available_tokens;
        if available >= MIN_LOCAL_NUM_CTX {
            *context = Ok(());
            return current.min(available);
        }
    }
    current
}

/// Startup: [`clamp_probe_result`] applied to the shared value.
pub fn clamp_local_num_ctx_to_probe(context: &mut Result<(), ModelError>) -> usize {
    let clamped = clamp_probe_result(local_num_ctx(), context);
    LOCAL_NUM_CTX.store(clamped, Ordering::Relaxed);
    clamped
}

/// PURE: `text` cut at a char boundary to the bytes that fit `num_ctx` after `reserve_tokens`
/// (3 bytes per token), with a marker appended when cut.
pub fn fit_text_to(text: &str, num_ctx: usize, reserve_tokens: usize) -> Cow<'_, str> {
    let budget = num_ctx.saturating_sub(reserve_tokens) * 3;
    if text.len() <= budget {
        return Cow::Borrowed(text);
    }
    let mut end = budget;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Cow::Owned(format!("{}{TRUNCATION_MARKER}", &text[..end]))
}

/// [`fit_text_to`] against the shared `num_ctx`.
pub fn fit_text(text: &str, reserve_tokens: usize) -> Cow<'_, str> {
    fit_text_to(text, local_num_ctx(), reserve_tokens)
}

/// Tokens kept free for the model's answer when a prompt is fitted.
const ANSWER_RESERVE_TOKENS: usize = 1024;

/// PURE: `prompt` with only its variable data cut to fit `num_ctx`. `instructions` is the standing
/// text: when the prompt carries it, everything up to its end is kept verbatim and only what follows is
/// cut; when the prompt is data alone, the instructions (sent beside it) are reserved for. A prompt that
/// already fits comes back byte-identical.
pub fn fit_prompt_data<'a>(prompt: &'a str, instructions: &str, num_ctx: usize) -> Cow<'a, str> {
    let reserve = ANSWER_RESERVE_TOKENS + instructions.len() / 3 + 1;
    let split = if instructions.is_empty() {
        None
    } else {
        prompt.find(instructions).map(|at| at + instructions.len())
    };
    match split {
        Some(end) => {
            let (head, data) = prompt.split_at(end);
            match fit_text_to(data, num_ctx, reserve) {
                Cow::Borrowed(_) => Cow::Borrowed(prompt),
                Cow::Owned(cut) => Cow::Owned(format!("{head}{cut}")),
            }
        }
        None => fit_text_to(prompt, num_ctx, reserve),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execution::runner::ModelError;

    #[test]
    fn resolve_defaults_when_nothing_is_declared() {
        assert_eq!(resolve_local_num_ctx(None, None), 8192);
        assert_eq!(DEFAULT_LOCAL_NUM_CTX, 8192);
        assert_eq!(MIN_LOCAL_NUM_CTX, 4096);
    }

    #[test]
    fn resolve_honours_a_declared_value() {
        assert_eq!(resolve_local_num_ctx(Some(16384), None), 16384);
    }

    #[test]
    fn resolve_is_capped_by_the_probed_window() {
        assert_eq!(resolve_local_num_ctx(Some(32768), Some(16384)), 16384);
        assert_eq!(resolve_local_num_ctx(None, Some(4096)), 4096);
    }

    #[test]
    fn resolve_never_grows_past_the_default_because_of_a_large_window() {
        assert_eq!(resolve_local_num_ctx(None, Some(40960)), 8192);
    }

    #[test]
    fn global_defaults_to_the_shared_default_in_tests() {
        assert_eq!(local_num_ctx(), 8192);
    }

    #[test]
    fn clamp_probe_accepts_a_window_above_the_floor_and_shrinks_the_value() {
        let mut context = Err(ModelError::ContextTooSmall {
            available_tokens: 6144,
            required_tokens: 8192,
        });
        assert_eq!(clamp_probe_result(8192, &mut context), 6144);
        assert_eq!(context, Ok(()));
    }

    #[test]
    fn clamp_probe_keeps_the_error_below_the_floor() {
        let too_small = ModelError::ContextTooSmall {
            available_tokens: 2048,
            required_tokens: 4096,
        };
        let mut context = Err(too_small.clone());
        assert_eq!(clamp_probe_result(8192, &mut context), 8192);
        assert_eq!(context, Err(too_small));
    }

    #[test]
    fn clamp_probe_leaves_an_ok_result_alone() {
        let mut context = Ok(());
        assert_eq!(clamp_probe_result(8192, &mut context), 8192);
        assert_eq!(context, Ok(()));
    }

    #[test]
    fn fit_text_to_borrows_a_short_text_unchanged() {
        let fitted = fit_text_to("hello", 8192, 2048);
        assert!(matches!(fitted, std::borrow::Cow::Borrowed("hello")));
    }

    #[test]
    fn fit_text_to_cuts_at_the_byte_budget_and_appends_the_marker() {
        let text = "a".repeat(100_000);
        let fitted = fit_text_to(&text, 8192, 2048);
        let marker = "\n[truncated to fit the local model's context]";
        assert_eq!(fitted.len(), 18_432 + marker.len());
        assert!(fitted.starts_with(&text[..18_432]));
        assert!(fitted.ends_with(marker));
    }

    #[test]
    fn fit_text_to_never_splits_a_multibyte_char() {
        let text = "é".repeat(50_000);
        let fitted = fit_text_to(&text, 8192, 2048);
        let marker = "\n[truncated to fit the local model's context]";
        let kept = fitted.strip_suffix(marker).expect("marker appended");
        assert!(kept.len() <= 18_432);
        assert!(text.starts_with(kept));
        assert!(std::str::from_utf8(fitted.as_bytes()).is_ok());
    }

    #[test]
    fn fit_prompt_data_keeps_the_instructions_and_cuts_only_the_data() {
        let instructions = "INSTRUCTIONS: answer in JSON.";
        let prompt = format!("{instructions}

--- DATA ---
{}", "x".repeat(100_000));
        let fitted = fit_prompt_data(&prompt, instructions, 8192);
        assert!(fitted.starts_with(&format!("{instructions}

--- DATA ---
")));
        assert!(fitted.ends_with("[truncated to fit the local model's context]"));
        assert!(fitted.len() < 8192 * 3);
    }

    #[test]
    fn fit_prompt_data_leaves_a_small_prompt_byte_identical() {
        let prompt = "INSTRUCTIONS

small data";
        let fitted = fit_prompt_data(prompt, "INSTRUCTIONS", 8192);
        assert!(matches!(fitted, std::borrow::Cow::Borrowed(p) if p == prompt));
    }

    #[test]
    fn fit_prompt_data_reserves_room_for_instructions_sent_beside_a_data_only_prompt() {
        let instructions = "i".repeat(3000);
        let data = "d".repeat(100_000);
        let fitted = fit_prompt_data(&data, &instructions, 8192);
        let marker = "
[truncated to fit the local model's context]";
        let kept = fitted.strip_suffix(marker).expect("marker appended");
        assert_eq!(kept.len(), (8192 - 1024 - 1001) * 3);
    }

    #[test]
    fn the_declared_window_sets_the_shared_value_only_for_the_ollama_engine() {
        assert_eq!(declared_ollama_num_ctx(None, Some(16384)), Some(16384));
        assert_eq!(declared_ollama_num_ctx(Some("ollama"), Some(16384)), Some(16384));
        assert_eq!(declared_ollama_num_ctx(Some("openai_compatible"), Some(16384)), None);
        assert_eq!(declared_ollama_num_ctx(Some("typo"), Some(16384)), None);
        assert_eq!(declared_ollama_num_ctx(None, None), None);
        assert_eq!(
            resolve_local_num_ctx(declared_ollama_num_ctx(Some("openai_compatible"), Some(65536)), None),
            8192
        );
    }

    #[test]
    fn distinct_models_drops_absent_and_repeated_names() {
        let named = [Some("a"), None, Some("b"), Some("a"), Some("b"), Some("c")];
        assert_eq!(distinct_models(&named), vec!["a", "b", "c"]);
        assert!(distinct_models(&[None, None]).is_empty());
    }
}
