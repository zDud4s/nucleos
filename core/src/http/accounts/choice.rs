//! What a request asks for: the default choice for new chats and runs, and a chat's own choice.
//! A choice is `auto`, `provider:<p>` or `account:<id>`; a named provider or account must exist.

use super::*;
use crate::accounts::settings::{AccountPoolDoc, current};
use crate::accounts::{AccountChoice, AccountId};
use crate::settings::{self as settings_store, Scope as SettingsScope, SettingsError};

#[derive(Deserialize)]
pub(super) struct ChoiceBody {
    choice: AccountChoice,
}

async fn check_exists(state: &AppState, choice: &AccountChoice) -> Result<(), Refused> {
    match choice {
        AccountChoice::Auto => Ok(()),
        AccountChoice::Provider(provider) => {
            let accounts = store::list(&state.pool).await.map_err(internal)?;
            if accounts.iter().any(|a| &a.provider == provider) {
                Ok(())
            } else {
                Err(refusal(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "unknown_provider",
                ))
            }
        }
        AccountChoice::Account(id) => match store::get(&state.pool, id).await.map_err(internal)? {
            Some(_) => Ok(()),
            None => Err(refusal(StatusCode::UNPROCESSABLE_ENTITY, "unknown_account")),
        },
    }
}

pub(super) async fn read_default() -> Json<Value> {
    Json(json!({ "choice": current().default_choice }))
}

pub(super) async fn write_default(
    State(state): State<AppState>,
    Json(body): Json<ChoiceBody>,
) -> Result<Json<Value>, Refused> {
    check_exists(&state, &body.choice).await?;
    set_default(&state, body.choice.clone()).await?;
    Ok(Json(json!({ "choice": body.choice })))
}

/// Writes `default_choice` into the `account_pool` document, other fields untouched.
pub(super) async fn set_default(state: &AppState, choice: AccountChoice) -> Result<(), Refused> {
    settings_store::update::<AccountPoolDoc, _>(
        &state.pool,
        &SettingsScope::Machine,
        "http:accounts",
        |doc| doc.default_choice = choice,
    )
    .await
    .map(|_| ())
    .map_err(|error| match error {
        SettingsError::Invalid(detail) => {
            refusal_with_detail(StatusCode::UNPROCESSABLE_ENTITY, "invalid", detail)
        }
        SettingsError::Stale { .. } => refusal(StatusCode::CONFLICT, "stale"),
        SettingsError::Unreadable { reason, .. } => {
            refusal_with_detail(StatusCode::UNPROCESSABLE_ENTITY, "unreadable", reason)
        }
        other => {
            tracing::warn!(error = %other, "the account default could not be written");
            refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
        }
    })
}

async fn chat_exists(state: &AppState, chat_id: &str) -> Result<(), Refused> {
    let found: Option<i64> = sqlx::query_scalar("SELECT 1 FROM chats WHERE chat_id = ?")
        .bind(chat_id)
        .fetch_optional(&state.pool)
        .await
        .map_err(internal)?;
    found
        .map(|_| ())
        .ok_or_else(|| refusal(StatusCode::NOT_FOUND, "no_such_chat"))
}

fn ids(row: Option<&AccountId>) -> Value {
    row.map_or(Value::Null, |id| json!(id))
}

/// A chat's choice and where it ran last. A chat with no row follows the default.
pub(super) async fn read_chat(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
) -> Result<Json<Value>, Refused> {
    chat_exists(&state, &chat_id).await?;
    let row = store::chat_account(&state.pool, &chat_id)
        .await
        .map_err(internal)?;
    Ok(Json(match row {
        Some(row) => json!({
            "chat_id": chat_id,
            "choice": row.choice,
            "inherited": false,
            "current_account_id": ids(row.current_account_id.as_ref()),
            "previous_account_id": ids(row.previous_account_id.as_ref()),
            "switched_at": row.switched_at,
        }),
        None => json!({
            "chat_id": chat_id,
            "choice": current().default_choice,
            "inherited": true,
            "current_account_id": null,
            "previous_account_id": null,
            "switched_at": null,
        }),
    }))
}

/// Pins the chat (or sets it back to `auto`). Takes effect at the next turn.
pub(super) async fn write_chat(
    State(state): State<AppState>,
    Path(chat_id): Path<String>,
    Json(body): Json<ChoiceBody>,
) -> Result<Json<Value>, Refused> {
    chat_exists(&state, &chat_id).await?;
    check_exists(&state, &body.choice).await?;
    store::set_chat_choice(&state.pool, &chat_id, &body.choice)
        .await
        .map_err(internal)?;
    Ok(Json(json!({ "chat_id": chat_id, "choice": body.choice })))
}
