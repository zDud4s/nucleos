//! The state-bearing slash built-ins: `/clear`, `/fresh`, `/context`, `/cost`, `/stop`, and the
//! dispatch of the six configuration ones to `builtins_config`.
//!
//! They live beside the route and not in `assistant::slash` because they call into the HTTP layer's
//! own state (the pool, the slots, the daemon's model config). `/help` stays in `command.rs`.

use crate::assistant::slash::capabilities::engine_of;
use crate::assistant::slash::parse::Parsed;
use crate::assistant::slash::{CardKind, Outcome, SlashError};
use crate::state::AppState;
use serde_json::{Value, json};

/// Whether `name` (as typed) is one of the built-ins this file and `builtins_config` run.
pub(super) fn handles(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "clear"
            | "fresh"
            | "context"
            | "cost"
            | "stop"
            | "model"
            | "effort"
            | "permissions"
            | "project"
            | "add-dir"
            | "budget"
    )
}

/// Runs the built-in `parsed` names. Call only when [`handles`] said yes.
pub(super) async fn run(
    state: &AppState,
    chat_id: &str,
    parsed: &Parsed,
) -> Result<Outcome, SlashError> {
    match parsed.name.to_lowercase().as_str() {
        "clear" => {
            no_arguments(parsed, "/clear")?;
            clear(state, chat_id).await
        }
        "fresh" => {
            no_arguments(parsed, "/fresh")?;
            fresh(state, chat_id).await
        }
        "context" => {
            no_arguments(parsed, "/context")?;
            context(state, chat_id).await
        }
        "cost" => {
            no_arguments(parsed, "/cost")?;
            cost(state, chat_id).await
        }
        "stop" => {
            no_arguments(parsed, "/stop")?;
            stop(state, chat_id).await
        }
        "model" | "effort" | "permissions" | "project" | "add-dir" | "budget" => {
            super::builtins_config::run(state, chat_id, parsed).await
        }
        other => Err(SlashError::Internal(format!("no built-in named /{other}"))),
    }
}

pub(super) fn internal(error: impl std::fmt::Display) -> SlashError {
    SlashError::Internal(error.to_string())
}

fn no_arguments(parsed: &Parsed, usage: &str) -> Result<(), SlashError> {
    if parsed.rest.is_empty() {
        Ok(())
    } else {
        Err(SlashError::InvalidArgument {
            usage: usage.to_string(),
            detail: format!("usage: {usage} (it takes no arguments)"),
        })
    }
}

pub(super) fn card(kind: CardKind, data: Value) -> Outcome {
    Outcome::Card { kind, data }
}

pub(super) fn notice(text: &str) -> Outcome {
    card(CardKind::Notice, json!({ "text": text }))
}

/// `/clear`: moves the replay floor to now and forgets the session, as `POST .../clear-context`.
async fn clear(state: &AppState, chat_id: &str) -> Result<Outcome, SlashError> {
    // A turn in flight already has a row and writes its session back when it ends.
    if crate::assistant::is_busy(chat_id) {
        return Err(SlashError::Busy);
    }
    crate::chats::clear_context(&state.pool, chat_id)
        .await
        .map_err(internal)?;
    // Both, in this order: the cut decides what a fresh turn is told, forgetting the session is
    // what makes the next turn fresh at all.
    crate::assistant::forget_session(&state.pool, chat_id)
        .await
        .map_err(internal)?;
    Ok(notice("Context cleared. The next message starts clean."))
}

/// `/fresh`: forgets the session only, so the next turn is replayed what was recently said.
async fn fresh(state: &AppState, chat_id: &str) -> Result<Outcome, SlashError> {
    if crate::assistant::is_busy(chat_id) {
        return Err(SlashError::Busy);
    }
    crate::assistant::forget_session(&state.pool, chat_id)
        .await
        .map_err(internal)?;
    Ok(notice(
        "Fresh context. The next message starts a new session with a short replay.",
    ))
}

/// `/stop`: interrupts the running turn. Nothing running is a notice, not an error.
async fn stop(state: &AppState, chat_id: &str) -> Result<Outcome, SlashError> {
    use crate::assistant::Stopped;
    Ok(match crate::assistant::stop_chat(state, chat_id).await {
        Some((_, Stopped::Interrupted)) => card(
            CardKind::Notice,
            json!({ "text": "Stopped the running turn.", "stopped": "interrupted" }),
        ),
        Some((_, Stopped::Killed)) => card(
            CardKind::Notice,
            json!({ "text": "Stopped the running turn.", "stopped": "killed" }),
        ),
        Some((_, Stopped::NotRunning)) | None => notice("Nothing running"),
    })
}

/// The chat row, or an internal error when it vanished between the route's check and now.
pub(super) async fn summary(
    state: &AppState,
    chat_id: &str,
) -> Result<crate::chats::ChatSummary, SlashError> {
    crate::chats::get(&state.pool, chat_id)
        .await
        .map_err(internal)?
        .ok_or_else(|| internal("the conversation is gone"))
}

/// One assistant turn as `/context` reads it.
type TurnRow = (String, Option<String>, i64, i64, i64, i64, Option<i64>, i64);

/// `/context`: what the window holds, measured off the newest finished turn.
async fn context(state: &AppState, chat_id: &str) -> Result<Outcome, SlashError> {
    let chat = summary(state, chat_id).await?;
    let brain = crate::chats::Brain::from_wire(&chat.brain);
    let engine = engine_of(
        &crate::config::models_config_now(),
        brain,
        chat.model.as_deref(),
    );
    let window = crate::assistant::window_of(chat.context_window);

    let rows: Vec<TurnRow> = sqlx::query_as(
        "SELECT status, session_id,
                COALESCE(input_tokens, 0), COALESCE(output_tokens, 0),
                COALESCE(cache_read_tokens, 0), COALESCE(cache_creation_tokens, 0),
                context_fill, compacted
           FROM runs
          WHERE chat_id = ? AND mode = 'assistant' AND id > COALESCE(?, -1)
          ORDER BY id",
    )
    .bind(chat_id)
    .bind(chat.cleared_after_run_id)
    .fetch_all(&state.pool)
    .await
    .map_err(internal)?;
    let live_session = crate::assistant::get_session(&state.pool, chat_id)
        .await
        .map_err(internal)?
        .is_some();

    let mut data = json!({
        "engine": engine.as_str(),
        "model": chat.model,
        "window_tokens": window,
        "used_tokens": null,
        "peak_tokens": null,
        "percent": null,
        "input_tokens": null,
        "output_tokens": null,
        "cache_read_tokens": null,
        "cache_creation_tokens": null,
        "live_session": live_session,
        "compacted": false,
        "turns": rows.len(),
    });

    use crate::assistant::slash::capabilities::ChatEngine;
    if matches!(engine, ChatEngine::Local | ChatEngine::OpenRouter) {
        data["note"] = json!("no measure on this engine");
        return Ok(card(CardKind::Context, data));
    }

    // The token columns are aggregates over every model call of a turn; the window's fill is the
    // per-call `context_fill`, so that is what `used` and `peak` read.
    let finished: Vec<&TurnRow> = rows.iter().filter(|row| row.0 != "running").collect();
    if let Some(last) = finished.last() {
        data["input_tokens"] = json!(last.2);
        data["output_tokens"] = json!(last.3);
        data["cache_read_tokens"] = json!(last.4);
        data["cache_creation_tokens"] = json!(last.5);
    }
    if let Some(newest) = finished.iter().rev().find(|row| row.6.is_some()) {
        let used = newest.6.unwrap_or(0);
        // The peak is over the session the newest measured turn ran in.
        let in_session = || finished.iter().filter(|row| row.1 == newest.1);
        let peak = in_session().filter_map(|row| row.6).max().unwrap_or(used);
        data["used_tokens"] = json!(used);
        data["peak_tokens"] = json!(peak);
        data["percent"] = json!((used as f64 * 1000.0 / window as f64).round() / 10.0);
        data["compacted"] = json!(in_session().any(|row| row.7 != 0));
    }
    Ok(card(CardKind::Context, data))
}

/// `/cost`: what the conversation has spent, from its runs.
async fn cost(state: &AppState, chat_id: &str) -> Result<Outcome, SlashError> {
    let chat = summary(state, chat_id).await?;
    let spent = crate::chats::cost_of(&state.pool, chat_id)
        .await
        .map_err(internal)?;
    Ok(card(
        CardKind::Cost,
        json!({
            "total_usd": spent.total_usd,
            "turns": spent.turns,
            "unmeasured_turns": spent.unmeasured_turns,
            "since_clear_usd": spent.since_clear_usd,
            "input_tokens": spent.input_tokens,
            "output_tokens": spent.output_tokens,
            "last_turn_usd": spent.last_turn_usd,
            "turn_budget_usd": chat.turn_budget_usd,
        }),
    ))
}
