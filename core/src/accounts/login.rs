//! Guided login: a new account is created by running the provider's own login command in the
//! account's own directory. W0 declares the registry and its views; the login branch spawns the
//! process. The daemon opens no browser: it surfaces `auth_url` and the shell opens it.

use std::path::Path;

use serde::Serialize;
use sqlx::SqlitePool;

use super::AccountId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LoginPhase {
    Starting,
    AwaitingBrowser,
    AwaitingCode,
    Verifying,
    Done,
    Failed,
    Cancelled,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoginView {
    pub login_id: String,
    pub account_id: String,
    pub engine: String,
    pub label: String,
    pub phase: LoginPhase,
    pub auth_url: Option<String>,
    pub message: Option<String>,
}

/// Registry of in-flight logins.
pub struct Logins {}

const NOT_YET: &str = "guided login is not available yet";

impl Logins {
    pub fn global() -> &'static Logins {
        static LOGINS: Logins = Logins {};
        &LOGINS
    }

    /// New account: creates its directory, runs the login command, and on success inserts the row.
    /// Failed or cancelled: the directory is removed and no row exists.
    pub async fn start_new(
        &self,
        _db: &SqlitePool,
        _data_dir: &Path,
        _engine: &str,
        _label: &str,
    ) -> Result<LoginView, String> {
        Err(NOT_YET.to_owned())
    }

    /// Existing account (re-login): same directory, never deleted on failure; done => active.
    pub async fn start_relogin(
        &self,
        _db: &SqlitePool,
        _id: &AccountId,
    ) -> Result<LoginView, String> {
        Err(NOT_YET.to_owned())
    }

    pub fn view(&self, _login_id: &str) -> Option<LoginView> {
        None
    }

    /// Writes the code and a newline to the login process' stdin.
    pub async fn submit_code(&self, _login_id: &str, _code: &str) -> Result<LoginView, String> {
        Err(NOT_YET.to_owned())
    }

    pub async fn cancel(&self, _login_id: &str) -> Option<LoginView> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_serialize_in_snake_case() {
        assert_eq!(
            serde_json::to_string(&LoginPhase::AwaitingBrowser).unwrap(),
            "\"awaiting_browser\""
        );
        assert!(Logins::global().view("nope").is_none());
    }
}
