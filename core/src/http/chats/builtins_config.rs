//! The configuration slash built-ins: `/model`, `/effort`, `/permissions`, `/project`, `/add-dir`
//! and `/budget`. `builtins::run` hands `parsed` here when `parsed.name` is one of those six.
//!
//! Every edit goes through `patch_chat`, the handler behind `PATCH /assistant/chats/{id}`, with a
//! request built from JSON, so the validation (`checked_dirs`, `checked_denials`, `checked_budget`,
//! the effort levels, the busy guard) lives in one place. What this file adds is the part the
//! patch does not know: the engine table of spec section 6, the busy guard for the two fields the
//! patch leaves unguarded, and the argument syntax.

use super::builtins::{card, internal, notice, summary};
use crate::assistant::slash::capabilities::{
    Capability, ChatEngine, Support, engine_of, require, support,
};
use crate::assistant::slash::model_pick::resolve_model;
use crate::assistant::slash::parse::Parsed;
use crate::assistant::slash::{CardKind, Control, Outcome, SlashError};
use crate::http::chat_edit::{ChatRefusal, PatchChatRequest, patch_chat};
use crate::http::common::{Asking, menu};
use crate::state::AppState;
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::{Value, json};

/// Runs one of the six configuration built-ins (`parsed.name`, any case) against `chat_id`.
pub(super) async fn run(
    state: &AppState,
    chat_id: &str,
    parsed: &Parsed,
) -> Result<Outcome, SlashError> {
    match parsed.name.to_lowercase().as_str() {
        "model" => model(state, chat_id, parsed).await,
        "effort" => effort(state, chat_id, parsed).await,
        "permissions" => permissions(state, chat_id, parsed).await,
        "project" => project(state, chat_id, parsed).await,
        "add-dir" => add_dir(state, chat_id, parsed).await,
        "budget" => budget(state, chat_id, parsed).await,
        other => Err(SlashError::Internal(format!(
            "no configuration built-in /{other}"
        ))),
    }
}

fn invalid(usage: &str, detail: impl Into<String>) -> SlashError {
    SlashError::InvalidArgument {
        usage: usage.to_string(),
        detail: detail.into(),
    }
}

/// The engine answering the chat now, from its brain and pinned model.
fn engine_for(chat: &crate::chats::ChatSummary) -> ChatEngine {
    engine_of(
        &crate::config::models_config_now(),
        crate::chats::Brain::from_wire(&chat.brain),
        chat.model.as_deref(),
    )
}

/// Applies `body` through `patch_chat`. A bare 400 reads as `fallback_detail`: the patch says no
/// without saying why, and the person typing a command should be told what was wrong.
async fn patch(
    state: &AppState,
    chat_id: &str,
    body: Value,
    usage: &str,
    fallback_detail: &str,
) -> Result<(), SlashError> {
    let request: PatchChatRequest = serde_json::from_value(body).map_err(internal)?;
    let refusal = match patch_chat(
        State(state.clone()),
        Path(chat_id.to_string()),
        Json(request),
    )
    .await
    {
        Ok(_) => return Ok(()),
        Err(refusal) => refusal,
    };
    let (status, detail) = match refusal {
        ChatRefusal::Status(status) => (status, None),
        ChatRefusal::WithDetail(status, detail) => (status, Some(detail)),
        ChatRefusal::Coded(status, _, detail) => (status, Some(detail)),
    };
    Err(match status {
        StatusCode::CONFLICT => SlashError::Busy,
        // 503 is "this machine cannot serve that model", and its reason is for the person.
        StatusCode::BAD_REQUEST | StatusCode::SERVICE_UNAVAILABLE => {
            invalid(usage, detail.unwrap_or_else(|| fallback_detail.to_string()))
        }
        other => internal(format!("editing the conversation answered {other}")),
    })
}

fn check_idle(chat_id: &str) -> Result<(), SlashError> {
    if crate::assistant::is_busy(chat_id) {
        Err(SlashError::Busy)
    } else {
        Ok(())
    }
}

/// `/model [<id|label|prefix>|default]`
async fn model(state: &AppState, chat_id: &str, parsed: &Parsed) -> Result<Outcome, SlashError> {
    const USAGE: &str = "/model [<model>|default]";
    if parsed.argv.is_empty() {
        return Ok(Outcome::Open {
            control: Control::ModelMenu,
        });
    }
    let (_, choices) = menu(Asking::Chat).await;
    let picked =
        resolve_model(&choices, &parsed.argv.join(" ")).map_err(|detail| invalid(USAGE, detail))?;
    patch(
        state,
        chat_id,
        json!({ "model": picked }),
        USAGE,
        "that model cannot be used here",
    )
    .await?;
    Ok(notice(&match picked {
        Some(id) => format!("Model set to {id}. The next message starts a new session."),
        None => "Model unpinned. The conversation follows the default model.".to_string(),
    }))
}

/// `/effort [<level>|default]`
async fn effort(state: &AppState, chat_id: &str, parsed: &Parsed) -> Result<Outcome, SlashError> {
    const USAGE: &str = "/effort [<level>|default]";
    let chat = summary(state, chat_id).await?;
    let engine = engine_for(&chat);
    let config = crate::config::models_config_now();

    // The levels the pinned model takes: its own list, else the catalogue's union.
    let levels: Vec<String> = if support(engine, Capability::Effort) == Support::No {
        Vec::new()
    } else {
        chat.model
            .as_deref()
            .and_then(|id| config.catalogue().into_iter().find(|c| c.id == id))
            .map(|c| c.efforts)
            .filter(|efforts| !efforts.is_empty())
            .unwrap_or_else(|| config.effort_levels())
    };

    let Some(wanted) = parsed.argv.first() else {
        return Ok(card(
            CardKind::Effort,
            json!({
                "engine": engine.as_str(),
                "model": chat.model,
                "current": chat.effort,
                "efforts": levels,
            }),
        ));
    };
    if parsed.argv.len() > 1 {
        return Err(invalid(USAGE, format!("usage: {USAGE}")));
    }
    let level = wanted.to_lowercase();
    if level != "default" {
        require(engine, Capability::Effort, "set an effort")?;
    }
    let value = (level != "default").then_some(level.clone());
    let detail = format!(
        "unknown effort \"{wanted}\"; try one of: {}, or default",
        config.effort_levels().join(", ")
    );
    patch(state, chat_id, json!({ "effort": value }), USAGE, &detail).await?;
    Ok(notice(&match value {
        Some(level) => format!("Effort set to {level}."),
        None => "Effort cleared. The CLI's own default applies.".to_string(),
    }))
}

/// Every permission mode, as the patch spells it.
const MODES: [crate::chats::PermissionMode; 6] = [
    crate::chats::PermissionMode::Manual,
    crate::chats::PermissionMode::AcceptEdits,
    crate::chats::PermissionMode::Plan,
    crate::chats::PermissionMode::Auto,
    crate::chats::PermissionMode::Bypass,
    crate::chats::PermissionMode::DontAsk,
];

/// `/permissions [<mode>|deny <Tool>...|allow <Tool>...]`
async fn permissions(
    state: &AppState,
    chat_id: &str,
    parsed: &Parsed,
) -> Result<Outcome, SlashError> {
    const USAGE: &str = "/permissions [<mode>|deny <Tool>|allow <Tool>]";
    let Some(first) = parsed.argv.first() else {
        return Ok(Outcome::Open {
            control: Control::PermissionMenu,
        });
    };
    let verb = first.to_lowercase();
    if verb == "deny" || verb == "allow" {
        let tools = &parsed.argv[1..];
        if tools.is_empty() {
            return Err(invalid(USAGE, format!("usage: /permissions {verb} <Tool>")));
        }
        let chat = summary(state, chat_id).await?;
        require(engine_for(&chat), Capability::DeniedTools, "deny a tool")?;
        // The patch does not guard `denied_tools`, a turn in flight having its own copy; the spec
        // still asks for the 409 here.
        check_idle(chat_id)?;
        let mut denied = crate::chats::answering(&state.pool, chat_id)
            .await
            .map_err(internal)?
            .denied_tools;
        if verb == "deny" {
            for tool in tools {
                if !denied.contains(tool) {
                    denied.push(tool.clone());
                }
            }
        } else {
            denied.retain(|have| !tools.iter().any(|tool| tool.eq_ignore_ascii_case(have)));
        }
        patch(
            state,
            chat_id,
            json!({ "denied_tools": denied }),
            USAGE,
            "that is not a built-in tool name",
        )
        .await?;
        return Ok(notice(&if denied.is_empty() {
            "No tools are denied.".to_string()
        } else {
            format!("Denied tools: {}.", denied.join(", "))
        }));
    }

    if parsed.argv.len() > 1 {
        return Err(invalid(USAGE, format!("usage: {USAGE}")));
    }
    let spelled = verb.replace('-', "_");
    let Some(mode) = MODES.iter().find(|mode| mode.as_str() == spelled) else {
        let names: Vec<&str> = MODES.iter().map(|mode| mode.as_str()).collect();
        return Err(invalid(
            USAGE,
            format!("unknown mode \"{first}\"; try one of: {}", names.join(", ")),
        ));
    };
    patch(
        state,
        chat_id,
        json!({ "permission_mode": mode.as_str() }),
        USAGE,
        "that mode cannot be set",
    )
    .await?;
    Ok(notice(&format!("Permissions set to {}.", mode.as_str())))
}

/// A path as typed, taken from the raw text so inner runs of spaces survive: trimmed, and one
/// surrounding pair of matching quotes removed.
fn unquoted(raw: &str) -> String {
    let raw = raw.trim();
    for quote in ['"', '\''] {
        if let Some(inner) = raw
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner.to_string();
        }
    }
    raw.to_string()
}

/// `/project [<path>]`
async fn project(state: &AppState, chat_id: &str, parsed: &Parsed) -> Result<Outcome, SlashError> {
    const USAGE: &str = "/project [<path>]";
    if parsed.argv.is_empty() {
        return Ok(Outcome::Open {
            control: Control::ProjectPicker,
        });
    }
    let path = unquoted(&parsed.rest);
    patch(
        state,
        chat_id,
        json!({ "cwd": path }),
        USAGE,
        "the project must be an absolute path to a directory that exists",
    )
    .await?;
    Ok(notice(&format!(
        "Project set to {path}. The next message starts a new session."
    )))
}

/// Compares two typed directory paths the way a person means them.
fn same_dir(a: &str, b: &str) -> bool {
    let norm = |p: &str| {
        let p = p.replace('\\', "/");
        let p = p.trim_end_matches('/').to_string();
        if cfg!(windows) { p.to_lowercase() } else { p }
    };
    norm(a) == norm(b)
}

/// `/add-dir [<path>|--remove <path>|remove <path>]`
async fn add_dir(state: &AppState, chat_id: &str, parsed: &Parsed) -> Result<Outcome, SlashError> {
    const USAGE: &str = "/add-dir [<path>|--remove <path>]";
    let chat = summary(state, chat_id).await?;
    let mut dirs = crate::chats::answering(&state.pool, chat_id)
        .await
        .map_err(internal)?
        .extra_dirs;
    if parsed.argv.is_empty() {
        return Ok(card(
            CardKind::Dirs,
            json!({ "cwd": chat.cwd, "extra_dirs": dirs }),
        ));
    }
    require(engine_for(&chat), Capability::ExtraDirs, "add a directory")?;
    check_idle(chat_id)?;

    let removing = matches!(
        parsed.argv[0].to_lowercase().as_str(),
        "--remove" | "remove"
    );
    let path = if removing {
        // Drop the leading verb from the raw text, not from argv, to keep the path's own spacing.
        let rest = parsed.rest.trim_start();
        unquoted(
            rest.split_once(char::is_whitespace)
                .map_or("", |(_, tail)| tail),
        )
    } else {
        unquoted(&parsed.rest)
    };
    if path.is_empty() {
        return Err(invalid(USAGE, format!("usage: {USAGE}")));
    }
    if removing {
        let Some(at) = dirs.iter().position(|have| same_dir(have, &path)) else {
            return Err(invalid(
                USAGE,
                format!("{path} is not one of the extra directories"),
            ));
        };
        dirs.remove(at);
    } else if dirs.iter().any(|have| same_dir(have, &path)) {
        return Ok(notice(&format!("{path} is already an extra directory.")));
    } else {
        dirs.push(path.clone());
    }
    patch(
        state,
        chat_id,
        json!({ "extra_dirs": dirs }),
        USAGE,
        "the directory must be an absolute path that exists",
    )
    .await?;
    Ok(notice(&if removing {
        format!("Removed {path} from the extra directories.")
    } else {
        format!("Added {path} to the extra directories.")
    }))
}

/// The line a card carries when the engine stores the ceiling and never enforces it.
fn budget_warning(engine: ChatEngine) -> Option<String> {
    (support(engine, Capability::TurnBudget) == Support::Warn).then(|| {
        format!(
            "The {} engine ignores the per-turn budget; it is saved all the same.",
            engine.as_str()
        )
    })
}

/// `/budget [<usd>|off]`
async fn budget(state: &AppState, chat_id: &str, parsed: &Parsed) -> Result<Outcome, SlashError> {
    const USAGE: &str = "/budget [<usd>|off]";
    let chat = summary(state, chat_id).await?;
    let engine = engine_for(&chat);

    let wanted = match parsed.argv.as_slice() {
        [] => None,
        [one] => Some(one.to_lowercase()),
        _ => return Err(invalid(USAGE, format!("usage: {USAGE}"))),
    };
    let Some(wanted) = wanted else {
        return budget_card(state, chat.turn_budget_usd, engine).await;
    };

    let ceiling = if wanted == "off" {
        None
    } else {
        let amount: f64 = wanted.trim_start_matches('$').parse().map_err(|_| {
            invalid(
                USAGE,
                format!("\"{wanted}\" is not an amount in dollars; use a number or off"),
            )
        })?;
        // A NaN would serialise as JSON null, which the patch reads as "clear the ceiling".
        if !amount.is_finite() {
            return Err(invalid(
                USAGE,
                "the amount must be a finite number of dollars",
            ));
        }
        require(engine, Capability::TurnBudget, "set a budget")?;
        Some(amount)
    };
    patch(
        state,
        chat_id,
        json!({ "turn_budget_usd": ceiling }),
        USAGE,
        "the budget must be more than zero dollars",
    )
    .await?;

    match budget_warning(engine) {
        Some(_) => budget_card(state, ceiling, engine).await,
        None => Ok(notice(&match ceiling {
            Some(amount) => format!("Each turn may spend at most ${amount}."),
            None => "Per-turn budget cleared.".to_string(),
        })),
    }
}

/// The budget card: this chat's per-turn ceiling beside the daemon's global budget.
async fn budget_card(
    state: &AppState,
    ceiling: Option<f64>,
    engine: ChatEngine,
) -> Result<Outcome, SlashError> {
    let global = crate::governance::budget::snapshot(&state.pool, chrono::Utc::now())
        .await
        .map_err(internal)?;
    Ok(card(
        CardKind::Budget,
        json!({
            "turn_budget_usd": ceiling,
            "warning": budget_warning(engine),
            "global": global,
        }),
    ))
}
