//! The LLM account pool: several logins per provider, one chosen per chat turn or run launch.
//! This module owns the shared vocabulary (ids, choice text, the launch marker); the decision,
//! the in-memory pool and the I/O live in the sibling files named below.

pub mod adapters;
pub mod choose;
pub mod inter;
pub mod launch_chat;
pub mod launch_run;
pub mod login;
pub mod pool;
pub mod provider;
pub mod settings;
pub mod startup;
pub mod store;
pub mod switch;
pub mod token;
pub mod usage;
pub mod watch;

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// An account id: `default:<engine>` or `<engine>:<8 hex>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AccountId(pub String);

impl AccountId {
    /// The id of the migrated login of an engine.
    pub fn default_for(engine: &str) -> Self {
        Self(format!("default:{engine}"))
    }
    pub fn is_default(&self) -> bool {
        self.0.starts_with("default:")
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountKind {
    Subscription,
    ApiKey,
    /// A pasted subscription token (`claude setup-token`). The token lives in the OS credential
    /// store (see `token`), never in the row; the locator is the account's empty config dir.
    Token,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountState {
    Active,
    Paused,
    NeedsLogin,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Account {
    pub id: AccountId,
    pub engine: String,
    pub provider: String,
    pub label: String,
    pub kind: AccountKind,
    pub credential_locator: Option<String>,
    pub state: AccountState,
    pub created_at: String,
    pub updated_at: String,
}

/// The owner's choice for a chat or run; its text form is the wire and column form:
/// `auto` | `provider:<provider>` | `account:<id>` (ids contain ':', so split on the FIRST ':').
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum AccountChoice {
    #[default]
    Auto,
    Provider(String),
    Account(AccountId),
}

impl std::str::FromStr for AccountChoice {
    type Err = String;
    fn from_str(text: &str) -> Result<Self, String> {
        if text == "auto" {
            return Ok(Self::Auto);
        }
        match text.split_once(':') {
            Some(("provider", rest)) if !rest.is_empty() => Ok(Self::Provider(rest.to_owned())),
            Some(("account", rest)) if !rest.is_empty() => {
                Ok(Self::Account(AccountId(rest.to_owned())))
            }
            _ => Err(format!(
                "`{text}` is not an account choice (expected auto, provider:<p> or account:<id>)"
            )),
        }
    }
}

impl std::fmt::Display for AccountChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => f.write_str("auto"),
            Self::Provider(p) => write!(f, "provider:{p}"),
            Self::Account(id) => write!(f, "account:{}", id.as_str()),
        }
    }
}

impl Serialize for AccountChoice {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for AccountChoice {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

impl schemars::JsonSchema for AccountChoice {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "AccountChoice".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "type": "string" })
    }
}

/// The env var every account-bound launch carries. Inert to the CLIs.
pub const ACCOUNT_ENV_MARKER: &str = "NUCLEOS_ACCOUNT_ID";

/// Which account a request's env names, if any.
pub fn account_of(env: &[(String, String)]) -> Option<&str> {
    env.iter()
        .find(|(key, _)| key == ACCOUNT_ENV_MARKER)
        .map(|(_, value)| value.as_str())
}

/// `<data_dir>/accounts/<id with ':' replaced by '_'>` — never inside a repository.
pub fn account_dir(data_dir: &Path, id: &AccountId) -> PathBuf {
    data_dir
        .join("accounts")
        .join(id.as_str().replace(':', "_"))
}

/// Feed kinds (the shell's state map registers each).
pub const NEEDS_LOGIN_KIND: &str = "account_needs_login";
pub const PINNED_BLOCKED_KIND: &str = "account_pinned_blocked";
pub const LIMIT_HIT_KIND: &str = "account_limit_hit";
pub const UNPINNED_KIND: &str = "account_unpinned";
/// The run pause source of an account-blocked run.
pub const PAUSE_SOURCE: &str = "account";

/// What a launch hook decided for one launch.
#[derive(Default)]
pub struct AccountPick {
    pub account_id: Option<AccountId>,
    /// `adapter.account_env` plus `(ACCOUNT_ENV_MARKER, id)`.
    pub env: Vec<(String, String)>,
    pub reservation: Option<pool::Reservation>,
    /// `PinnedBlocked` | `SessionStuck` | `NoCandidate`.
    pub blocked: Option<choose::Decision>,
}

impl AccountPick {
    /// Today's launch: no account, no env, no reservation.
    pub fn default_launch() -> Self {
        Self::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choice_text_round_trips_and_splits_on_the_first_colon() {
        for text in [
            "auto",
            "provider:anthropic",
            "account:claude:1a2b3c4d",
            "account:default:codex",
        ] {
            let choice: AccountChoice = text.parse().unwrap();
            assert_eq!(choice.to_string(), text);
            let json = serde_json::to_string(&choice).unwrap();
            assert_eq!(
                serde_json::from_str::<AccountChoice>(&json).unwrap(),
                choice
            );
        }
        assert_eq!(
            "account:claude:1a2b3c4d".parse::<AccountChoice>().unwrap(),
            AccountChoice::Account(AccountId("claude:1a2b3c4d".into()))
        );
        for bad in ["", "provider:", "account:", "other:x", "Auto"] {
            assert!(bad.parse::<AccountChoice>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn ids_markers_and_dirs() {
        let id = AccountId::default_for("claude");
        assert_eq!(id.as_str(), "default:claude");
        assert!(id.is_default());
        assert!(!AccountId("claude:1a2b3c4d".into()).is_default());
        let env = vec![
            ("A".to_owned(), "b".to_owned()),
            (ACCOUNT_ENV_MARKER.to_owned(), "x:1".to_owned()),
        ];
        assert_eq!(account_of(&env), Some("x:1"));
        assert_eq!(account_of(&[]), None);
        let dir = account_dir(Path::new("data"), &AccountId("claude:1a2b3c4d".into()));
        assert_eq!(
            dir,
            Path::new("data").join("accounts").join("claude_1a2b3c4d")
        );
    }

    #[test]
    fn the_default_launch_is_empty() {
        let pick = AccountPick::default_launch();
        assert!(pick.account_id.is_none() && pick.env.is_empty());
        assert!(pick.reservation.is_none() && pick.blocked.is_none());
    }
}
