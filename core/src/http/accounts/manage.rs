//! Per-account changes: rename, pause, resume, redo login, remove. Each change fans out through
//! `startup::accounts_changed`, so the pool, the usage feed and the sidecar's reader list follow.

use std::path::Path as FsPath;

use super::*;
use crate::accounts::login::clean_label;
use crate::accounts::pool::PoolState;
use crate::accounts::{AccountChoice, AccountId, AccountState, UNPINNED_KIND, account_dir, startup};

async fn account_or_404(state: &AppState, id: &str) -> Result<Account, Refused> {
    store::get(&state.pool, &AccountId(id.to_owned()))
        .await
        .map_err(internal)?
        .ok_or_else(|| refusal(StatusCode::NOT_FOUND, "no_such_account"))
}

async fn answer(state: &AppState, id: &str) -> Result<Json<Value>, Refused> {
    let account = account_or_404(state, id).await?;
    let view = PoolState::global().view();
    Ok(Json(account_json(&account, &view)))
}

#[derive(Deserialize)]
pub(super) struct Rename {
    label: String,
}

pub(super) async fn rename_account(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<Rename>,
) -> Result<Json<Value>, Refused> {
    let label = clean_label(&body.label)
        .map_err(|detail| refusal_with_detail(StatusCode::UNPROCESSABLE_ENTITY, "bad_label", detail))?;
    match store::rename(&state.pool, &AccountId(id.clone()), &label).await {
        Ok(true) => {}
        Ok(false) => return Err(refusal(StatusCode::NOT_FOUND, "no_such_account")),
        Err(sqlx::Error::Database(error)) if error.is_unique_violation() => {
            return Err(refusal(StatusCode::CONFLICT, "label_taken"));
        }
        Err(error) => return Err(internal(error)),
    }
    startup::accounts_changed(&state.pool).await;
    answer(&state, &id).await
}

/// Takes the account out of the pool. A chat pinned to it is handled by R2 (warn and propose),
/// never moved here.
pub(super) async fn pause_account(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Refused> {
    let account = account_or_404(&state, &id).await?;
    if account.state != AccountState::Paused {
        store::set_state(&state.pool, &account.id, AccountState::Paused)
            .await
            .map_err(internal)?;
        startup::accounts_changed(&state.pool).await;
    }
    answer(&state, &id).await
}

/// Puts a paused account back. A `needs_login` account comes back only through a new login.
pub(super) async fn resume_account(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Value>, Refused> {
    let account = account_or_404(&state, &id).await?;
    match account.state {
        AccountState::NeedsLogin => return Err(refusal(StatusCode::CONFLICT, "needs_login")),
        AccountState::Paused => {
            store::set_state(&state.pool, &account.id, AccountState::Active)
                .await
                .map_err(internal)?;
            startup::accounts_changed(&state.pool).await;
        }
        AccountState::Active => {}
    }
    answer(&state, &id).await
}

/// Redoes the login of an existing account: same directory, never deleted on failure.
pub(super) async fn relogin_account(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<(StatusCode, Json<LoginView>), Refused> {
    let account = account_or_404(&state, &id).await?;
    let view = Logins::global()
        .start_relogin(&state.pool, &account.id)
        .await
        .map_err(|detail| {
            refusal_with_detail(StatusCode::UNPROCESSABLE_ENTITY, "login_refused", detail)
        })?;
    Ok((StatusCode::ACCEPTED, Json(view)))
}

#[derive(Deserialize, Default)]
pub(super) struct RemoveQuery {
    #[serde(default)]
    force: bool,
}

/// Removes an account. Chats pinned to it go back to Auto, and so does a default that named it
/// (B5); the owner is told through the feed. An account with live sessions needs `force=true`
/// (the shell asks for confirmation). The `default:*` accounts cannot be removed.
pub(super) async fn remove_account(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<RemoveQuery>,
) -> Result<Json<Value>, Refused> {
    let account = account_or_404(&state, &id).await?;
    if account.id.is_default() {
        return Err(refusal(StatusCode::CONFLICT, "default_account"));
    }
    let live = PoolState::global().live_sessions(&account.id);
    if live > 0 && !query.force {
        return Err((
            StatusCode::CONFLICT,
            Json(json!({ "refusal": "live_sessions", "live_sessions": live })),
        ));
    }
    let unpinned = match store::remove(&state.pool, &account.id).await {
        Ok(Ok(chats)) => chats,
        Ok(Err(reason)) => {
            return Err(refusal_with_detail(
                StatusCode::CONFLICT,
                "not_removable",
                reason.to_owned(),
            ));
        }
        Err(error) => return Err(internal(error)),
    };
    let pinned_default = AccountChoice::Account(account.id.clone());
    let default_reset = crate::accounts::settings::current().default_choice == pinned_default;
    if default_reset {
        choice::set_default(&state, AccountChoice::Auto).await?;
    }
    if let Ok(data_dir) = data_dir() {
        remove_owned_dir(&data_dir, &account).await;
    }
    if !unpinned.is_empty() || default_reset {
        let summary = format!(
            "Account {} ({}) was removed: {} chat(s){} went back to Auto",
            account.label,
            account.engine,
            unpinned.len(),
            if default_reset { " and the default" } else { "" },
        );
        if let Err(error) =
            crate::feed::append(&state.pool, None, UNPINNED_KIND, &summary, None, None).await
        {
            tracing::warn!(%error, "account removal could not write the feed");
        }
    }
    startup::accounts_changed(&state.pool).await;
    Ok(Json(json!({
        "removed": account.id,
        "unpinned_chats": unpinned.len(),
        "default_reset": default_reset,
    })))
}

/// Deletes the account's directory, but only the one NucleOS made for it
/// (`<data_dir>/accounts/<id>`): a locator pointing anywhere else is the owner's folder.
pub(super) async fn remove_owned_dir(data_dir: &FsPath, account: &Account) {
    let Some(locator) = &account.credential_locator else {
        return;
    };
    let ours = account_dir(data_dir, &account.id);
    if FsPath::new(locator) != ours {
        return;
    }
    if let Err(error) = tokio::fs::remove_dir_all(&ours).await
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%error, "could not remove the directory of a removed account");
    }
}
