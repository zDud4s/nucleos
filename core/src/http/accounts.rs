//! The account pool's routes. Owner-only by omission from every scoped route table in `auth.rs`.
//!
//! - `GET /accounts`: every account with its usage windows and live sessions, plus `engines`,
//!   the engines a new account can be added for.
//! - `POST /accounts {engine, label}`: starts a guided login (202 + the login view). The account
//!   row exists only once the login succeeds.
//! - `POST /accounts/token {engine, label, token}`: adds a token account (201 + the account);
//!   `POST /accounts/{id}/token {token}` replaces its token. See `token.rs`.
//! - `GET|DELETE /account-logins/{login_id}`, `POST /account-logins/{login_id}/code {code}`.
//! - `POST /accounts/{id}/rename {label}`, `/pause`, `/resume`, `/login` (redo login),
//!   `/logout`, `DELETE /accounts/{id}[?force=true]`.
//! - `GET|PUT /account-pool/default {choice}`: the default for new chats and runs.
//! - `GET|PUT /assistant/chats/{chat_id}/account {choice}`.
//!
//! The login and default routes sit outside `/accounts/{id}` on purpose: a static segment beside a
//! parameter makes the router's matching order matter, and an account id contains `:`.

use serde_json::{Value, json};

use super::common::refusal_with_detail;
use super::*;
use crate::accounts::login::{LoginView, Logins};
use crate::accounts::pool::PoolView;
use crate::accounts::{Account, store};
use crate::door::refusal;

mod choice;
mod manage;
#[cfg(test)]
mod tests;
mod token;

type Refused = (StatusCode, Json<Value>);

pub(super) fn routes() -> Router<AppState> {
    Router::new()
        .route("/accounts", get(list_accounts).post(start_account))
        .route("/accounts/token", post(token::add_token_account))
        .route("/accounts/{id}", delete(manage::remove_account))
        .route("/accounts/{id}/token", post(token::replace_token))
        .route("/accounts/{id}/rename", post(manage::rename_account))
        .route("/accounts/{id}/pause", post(manage::pause_account))
        .route("/accounts/{id}/resume", post(manage::resume_account))
        .route("/accounts/{id}/login", post(manage::relogin_account))
        .route("/accounts/{id}/logout", post(manage::logout_account))
        .route(
            "/account-logins/{login_id}",
            get(read_login).delete(cancel_login),
        )
        .route("/account-logins/{login_id}/code", post(submit_code))
        .route(
            "/account-pool/default",
            get(choice::read_default).put(choice::write_default),
        )
        .route(
            "/assistant/chats/{chat_id}/account",
            get(choice::read_chat).put(choice::write_chat),
        )
}

fn internal(error: sqlx::Error) -> Refused {
    tracing::warn!(%error, "account pool: database error");
    refusal(StatusCode::INTERNAL_SERVER_ERROR, "internal")
}

fn data_dir() -> Result<std::path::PathBuf, Refused> {
    crate::data_dir::data_dir().map_err(|error| {
        tracing::warn!(%error, "account pool: no data directory");
        refusal(StatusCode::INTERNAL_SERVER_ERROR, "no_data_dir")
    })
}

/// The account as the shell reads it: no credential locator, usage and live sessions merged in.
pub(super) fn account_json(account: &Account, view: &PoolView) -> Value {
    let candidate = view.candidates.iter().find(|c| c.account.id == account.id);
    let usage = candidate.and_then(|c| c.usage.as_ref()).map(|usage| {
        json!({
            "source": usage.fidelity,
            "read_at": usage.read_at.map(|at| at.to_rfc3339()),
            "windows": usage.windows.iter().map(|w| json!({
                "name": w.name,
                "used_fraction": w.used_fraction,
                "resets_at": w.resets_at.map(|at| at.to_rfc3339()),
                "stale": w.stale,
            })).collect::<Vec<_>>(),
        })
    });
    json!({
        "id": account.id,
        "engine": account.engine,
        "provider": account.provider,
        "label": account.label,
        "kind": account.kind,
        "state": account.state,
        "is_default": account.id.is_default(),
        "created_at": account.created_at,
        "updated_at": account.updated_at,
        "live_sessions": candidate.map_or(0, |c| c.live_sessions),
        "usage": usage,
    })
}

async fn list_accounts(State(state): State<AppState>) -> Result<Json<Value>, Refused> {
    let accounts = store::list(&state.pool).await.map_err(internal)?;
    let view = crate::accounts::pool::PoolState::global().view();
    Ok(Json(json!({
        "accounts": accounts.iter().map(|a| account_json(a, &view)).collect::<Vec<_>>(),
        "engines": crate::accounts::adapters::pooled_engines(),
    })))
}

#[derive(Deserialize)]
struct StartAccount {
    engine: String,
    label: String,
}

/// Starts the guided login of a new account. The daemon opens no browser: the view carries the
/// `auth_url` once the CLI prints it, and the shell opens it.
async fn start_account(
    State(state): State<AppState>,
    Json(body): Json<StartAccount>,
) -> Result<(StatusCode, Json<LoginView>), Refused> {
    let data_dir = data_dir()?;
    let view = Logins::global()
        .start_new(&state.pool, &data_dir, &body.engine, &body.label)
        .await
        .map_err(|detail| {
            refusal_with_detail(StatusCode::UNPROCESSABLE_ENTITY, "login_refused", detail)
        })?;
    Ok((StatusCode::ACCEPTED, Json(view)))
}

async fn read_login(Path(login_id): Path<String>) -> Result<Json<LoginView>, Refused> {
    Logins::global()
        .view(&login_id)
        .map(Json)
        .ok_or_else(|| refusal(StatusCode::NOT_FOUND, "no_such_login"))
}

async fn cancel_login(Path(login_id): Path<String>) -> Result<Json<LoginView>, Refused> {
    Logins::global()
        .cancel(&login_id)
        .await
        .map(Json)
        .ok_or_else(|| refusal(StatusCode::NOT_FOUND, "no_such_login"))
}

#[derive(Deserialize)]
struct Code {
    code: String,
}

/// The code is written to the login process and goes nowhere else: no log, no store.
async fn submit_code(
    Path(login_id): Path<String>,
    Json(body): Json<Code>,
) -> Result<Json<LoginView>, Refused> {
    let logins = Logins::global();
    if logins.view(&login_id).is_none() {
        return Err(refusal(StatusCode::NOT_FOUND, "no_such_login"));
    }
    logins
        .submit_code(&login_id, &body.code)
        .await
        .map(Json)
        .map_err(|detail| refusal_with_detail(StatusCode::CONFLICT, "code_refused", detail))
}
