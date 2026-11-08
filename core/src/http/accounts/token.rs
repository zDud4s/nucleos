//! Token accounts: a Claude subscription token pasted by the owner (the output of
//! `claude setup-token`) instead of an interactive login.
//!
//! - `POST /accounts/token {engine, label, token}`: 201 + the account.
//! - `POST /accounts/{id}/token {token}`: replaces the token of a token account, 200 + the account.
//!
//! The token goes to the OS credential store (`accounts::token`) and nowhere else: it is not in the
//! row, not in a log, and the request bodies below derive no `Debug`.

use std::path::Path as FsPath;

use rand::RngExt;

use super::manage::{account_or_404, answer};
use super::*;
use crate::accounts::login::clean_label;
use crate::accounts::token::{self, clean_token};
use crate::accounts::{AccountId, AccountKind, AccountState, account_dir, startup};

/// The engine a token account can be added for: only Claude Code reads a pasted token.
const TOKEN_ENGINE: &str = crate::accounts::adapters::claude::ENGINE;

#[derive(Deserialize)]
pub(super) struct AddToken {
    pub(super) engine: String,
    pub(super) label: String,
    pub(super) token: String,
}

#[derive(Deserialize)]
pub(super) struct ReplaceToken {
    token: String,
}

fn empty_token() -> Refused {
    refusal(StatusCode::UNPROCESSABLE_ENTITY, "empty_token")
}

fn secret_store_failed(error: String) -> Refused {
    tracing::warn!(%error, "account pool: the credential store refused a token");
    refusal(StatusCode::INTERNAL_SERVER_ERROR, "secret_store_failed")
}

pub(super) async fn add_token_account(
    State(state): State<AppState>,
    Json(body): Json<AddToken>,
) -> Result<(StatusCode, Json<Value>), Refused> {
    let data_dir = data_dir()?;
    add_in(&state, &data_dir, body).await
}

/// `add_token_account` against an explicit data directory (the tests use a temp one).
pub(super) async fn add_in(
    state: &AppState,
    data_dir: &FsPath,
    body: AddToken,
) -> Result<(StatusCode, Json<Value>), Refused> {
    let adapter = crate::accounts::adapters::adapter_for(&body.engine)
        .filter(|_| body.engine == TOKEN_ENGINE)
        .ok_or_else(|| refusal(StatusCode::UNPROCESSABLE_ENTITY, "token_engine_unsupported"))?;
    let secret = clean_token(&body.token).ok_or_else(empty_token)?;
    let label_refused = |detail: String| {
        refusal_with_detail(StatusCode::UNPROCESSABLE_ENTITY, "login_refused", detail)
    };
    let label = clean_label(&body.label).map_err(label_refused)?;
    let taken = store::list(&state.pool).await.map_err(internal)?;
    if taken
        .iter()
        .any(|a| a.engine == body.engine && a.label == label)
    {
        return Err(label_refused(format!(
            "an account named `{label}` already exists for {}",
            body.engine
        )));
    }

    let id = AccountId(format!(
        "{}:{:08x}",
        body.engine,
        rand::rng().random::<u32>()
    ));
    let dir = account_dir(data_dir, &id);
    let now = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let account = Account {
        id: id.clone(),
        engine: body.engine.clone(),
        provider: adapter.provider().to_owned(),
        label,
        kind: AccountKind::Token,
        credential_locator: Some(dir.to_string_lossy().into_owned()),
        state: AccountState::Active,
        created_at: now.clone(),
        updated_at: now,
    };

    // The empty directory isolates the account's history (`CLAUDE_CONFIG_DIR`), as for a login.
    if let Err(error) = tokio::fs::create_dir_all(&dir).await {
        tracing::warn!(%error, "account pool: could not create a token account directory");
        return Err(refusal(
            StatusCode::INTERNAL_SERVER_ERROR,
            "account_dir_failed",
        ));
    }
    if let Err(error) = token::store(&id, &secret).await {
        remove_dir(&dir).await;
        return Err(secret_store_failed(error));
    }
    if let Err(error) = store::insert(&state.pool, &account).await {
        // No row, so nothing may keep the token or the directory.
        if let Err(error) = token::delete(&id).await {
            tracing::warn!(%error, "account pool: could not delete the token of a failed add");
        }
        remove_dir(&dir).await;
        return Err(match error {
            sqlx::Error::Database(db) if db.is_unique_violation() => {
                refusal(StatusCode::CONFLICT, "label_taken")
            }
            other => internal(other),
        });
    }
    startup::accounts_changed(&state.pool).await;
    let Json(account) = answer(state, id.as_str()).await?;
    Ok((StatusCode::CREATED, Json(account)))
}

async fn remove_dir(dir: &FsPath) {
    if let Err(error) = tokio::fs::remove_dir_all(dir).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, "account pool: could not remove the directory of a failed add");
    }
}

/// Replaces the token of a token account and puts it back to `active`: the way out of
/// `needs_login` for an account whose token was rejected or logged out.
pub(super) async fn replace_token(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ReplaceToken>,
) -> Result<Json<Value>, Refused> {
    let account = account_or_404(&state, &id).await?;
    if account.kind != AccountKind::Token {
        return Err(refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            "not_token_account",
        ));
    }
    let secret = clean_token(&body.token).ok_or_else(empty_token)?;
    token::store(&account.id, &secret)
        .await
        .map_err(secret_store_failed)?;
    store::set_state(&state.pool, &account.id, AccountState::Active)
        .await
        .map_err(internal)?;
    startup::accounts_changed(&state.pool).await;
    answer(&state, &id).await
}
