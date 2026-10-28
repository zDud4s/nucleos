//! The state-bearing slash built-ins: `/clear`, `/fresh`, `/context`, `/cost`, `/stop`, the
//! dispatch of the six configuration ones to `builtins_config`, and of `/mcp` to `mcp`.
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
            | "mcp"
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
        "mcp" => super::mcp::slash(state, chat_id, parsed).await,
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
    let stopped = crate::assistant::stop_chat(state, chat_id).await;
    // A killed turn's task is aborted before it settles its compaction, if it was one; an
    // interrupted one is still running and settles itself.
    crate::assistant::compaction::settle_stale(&state.pool, chat_id).await;
    Ok(match stopped {
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

/// One assistant turn as `/context` reads it; the run id is last.
type TurnRow = (
    String,
    Option<String>,
    i64,
    i64,
    i64,
    i64,
    Option<i64>,
    i64,
    i64,
);

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
                context_fill, compacted, id
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
    // The `/compact` handoff in force, on every engine: null when none or after a `/clear`.
    let active = crate::chats::compactions::active(&state.pool, chat_id)
        .await
        .map_err(internal)?;
    // The turns the window holds start after the later of the two floors: the `/clear` (already
    // applied by the query) and the compaction turn in force, whose handoff replaced everything
    // before it. `active` only ever returns a compaction above the clear floor.
    let turn_floor = active.as_ref().map_or(-1, |active| active.run_id);
    let turns = rows.iter().filter(|row| row.8 > turn_floor).count();
    let compaction =
        active.map(|active| json!({ "run_id": active.run_id, "at": active.created_at }));

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
        "compaction": compaction,
        "turns": turns,
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

#[cfg(test)]
mod tests {
    use crate::http::build_router;
    use crate::state::AppState;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use serde_json::{Value, json};
    use tower::ServiceExt;

    async fn context_turns(state: &AppState, chat: &str) -> Value {
        let response = build_router(state.clone())
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/assistant/chats/{chat}/command"))
                    .header("Authorization", "Bearer test-token")
                    .header("Content-Type", "application/json")
                    .body(Body::from(json!({ "text": "/context" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        assert_eq!(status, StatusCode::OK, "{body}");
        body["card"]["data"]["turns"].clone()
    }

    async fn seed_chat(state: &AppState, chat: &str) {
        sqlx::query(
            "INSERT INTO chats (chat_id, brain, created_at) VALUES (?, 'cloud', '2026-10-28T10:00:00Z')",
        )
        .bind(chat)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    /// `n` finished turns of `chat`; returns the id of the last one.
    async fn seed_turns(state: &AppState, chat: &str, n: usize) -> i64 {
        let mut last = 0;
        for _ in 0..n {
            last = sqlx::query_scalar(
                "INSERT INTO runs (prompt, status, mode, chat_id, stdout, created_at)
                 VALUES ('asked', 'completed', 'assistant', ?, 'answered',
                         '2026-10-28T10:00:00+00:00')
                 RETURNING id",
            )
            .bind(chat)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        }
        last
    }

    /// A settled (`done`) compaction whose turn is `run_id`.
    async fn seed_compaction(state: &AppState, chat: &str, run_id: i64) {
        crate::chats::compactions::insert_pending(
            &state.pool,
            chat,
            run_id,
            "v1",
            "claude",
            None,
            None,
        )
        .await
        .unwrap();
        sqlx::query(
            "UPDATE chat_compactions SET status = 'done', summary = 'HANDOFF' WHERE run_id = ?",
        )
        .bind(run_id)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    async fn set_clear_floor(state: &AppState, chat: &str, run_id: i64) {
        sqlx::query("UPDATE chats SET cleared_after_run_id = ? WHERE chat_id = ?")
            .bind(run_id)
            .bind(chat)
            .execute(&state.pool)
            .await
            .unwrap();
    }

    /// clear, 3 turns, a compaction turn, 2 turns: the window holds the 2 after the compaction.
    #[tokio::test]
    async fn context_counts_turns_from_the_compaction_in_force() {
        let state = crate::http::tests::test_state().await;
        let chat = "b-context-turns-after-compact";
        seed_chat(&state, chat).await;
        let cleared = seed_turns(&state, chat, 2).await;
        set_clear_floor(&state, chat, cleared).await;
        seed_turns(&state, chat, 3).await;
        assert_eq!(context_turns(&state, chat).await, json!(3));

        let compaction = seed_turns(&state, chat, 1).await;
        seed_compaction(&state, chat, compaction).await;
        seed_turns(&state, chat, 2).await;
        assert_eq!(context_turns(&state, chat).await, json!(2));
    }

    /// A compaction from before the `/clear` is not in force, so the clear floor alone counts.
    #[tokio::test]
    async fn context_ignores_a_compaction_older_than_the_clear() {
        let state = crate::http::tests::test_state().await;
        let chat = "b-context-turns-old-compact";
        seed_chat(&state, chat).await;
        let compaction = seed_turns(&state, chat, 1).await;
        seed_compaction(&state, chat, compaction).await;
        let cleared = seed_turns(&state, chat, 2).await;
        set_clear_floor(&state, chat, cleared).await;
        seed_turns(&state, chat, 4).await;
        assert_eq!(context_turns(&state, chat).await, json!(4));
    }
}
