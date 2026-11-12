//! The credential side of a token account: a subscription token pasted by the owner (the output of
//! `claude setup-token`). The token lives in the OS credential store under
//! `claude-account-token:<account id>` and nowhere else: not in the database, not in a log, not in
//! a `Debug` output.
//!
//! The credential store is synchronous, so every access here hops to a blocking thread (like
//! `judge/client.rs`). Under `cfg(test)`, and with the `testkit` feature (the tests of the crates
//! above, e.g. `nucleos-daemon`'s HTTP tests), the store is an in-process map: a test never touches
//! the desktop's real credentials.

use super::AccountId;

/// The env var Claude Code reads a subscription token from.
pub const ENV_VAR: &str = "CLAUDE_CODE_OAUTH_TOKEN";

const KEY_PREFIX: &str = "claude-account-token:";

/// The credential-store key of an account's token.
pub fn secret_key(id: &AccountId) -> String {
    format!("{KEY_PREFIX}{}", id.as_str())
}

/// The launch env entry that carries a token to the CLI.
pub fn env_pair(token: &str) -> (String, String) {
    (ENV_VAR.to_owned(), token.to_owned())
}

/// A pasted token, trimmed. `None` when nothing usable is left: empty, or holding whitespace or
/// control characters inside (an env var could not carry it, and it is no token).
pub fn clean_token(token: &str) -> Option<String> {
    let token = token.trim();
    if token.is_empty() || token.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return None;
    }
    Some(token.to_owned())
}

#[cfg(not(any(test, feature = "testkit")))]
mod backend {
    pub fn store(key: &str, value: &str) -> Result<(), String> {
        crate::secrets::store_secret(key, value).map_err(|error| error.to_string())
    }
    pub fn load(key: &str) -> Result<Option<String>, String> {
        crate::secrets::load_secret(key).map_err(|error| error.to_string())
    }
    pub fn delete(key: &str) -> Result<(), String> {
        crate::secrets::delete_secret(key).map_err(|error| error.to_string())
    }
}

#[cfg(any(test, feature = "testkit"))]
mod backend {
    use std::collections::HashMap;
    use std::sync::Mutex;

    static ENTRIES: Mutex<Option<HashMap<String, String>>> = Mutex::new(None);

    fn with<T>(change: impl FnOnce(&mut HashMap<String, String>) -> T) -> T {
        let mut entries = ENTRIES.lock().unwrap_or_else(|e| e.into_inner());
        change(entries.get_or_insert_with(HashMap::new))
    }

    pub fn store(key: &str, value: &str) -> Result<(), String> {
        with(|map| map.insert(key.to_owned(), value.to_owned()));
        Ok(())
    }
    pub fn load(key: &str) -> Result<Option<String>, String> {
        Ok(with(|map| map.get(key).cloned()))
    }
    pub fn delete(key: &str) -> Result<(), String> {
        with(|map| map.remove(key));
        Ok(())
    }
}

fn joined<T>(outcome: Result<Result<T, String>, tokio::task::JoinError>) -> Result<T, String> {
    outcome.map_err(|error| error.to_string())?
}

/// Stores (or replaces) the token of an account.
pub async fn store(id: &AccountId, token: &str) -> Result<(), String> {
    let (key, token) = (secret_key(id), token.to_owned());
    joined(tokio::task::spawn_blocking(move || backend::store(&key, &token)).await)
}

/// The stored token, `None` when there is none (or it is empty).
pub async fn load(id: &AccountId) -> Result<Option<String>, String> {
    let key = secret_key(id);
    let loaded = joined(tokio::task::spawn_blocking(move || backend::load(&key)).await)?;
    Ok(loaded.filter(|token| !token.is_empty()))
}

/// Deletes the token of an account. Deleting what is not there is fine.
pub async fn delete(id: &AccountId) -> Result<(), String> {
    let key = secret_key(id);
    joined(tokio::task::spawn_blocking(move || backend::delete(&key)).await)
}

/// The stored token, read synchronously. Tests only (here, and above through `testkit`).
#[cfg(any(test, feature = "testkit"))]
pub fn peek(id: &AccountId) -> Option<String> {
    backend::load(&secret_key(id)).unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_key_and_the_env_pair_follow_the_contract() {
        let id = AccountId("claude:0a0b0c0d".into());
        assert_eq!(secret_key(&id), "claude-account-token:claude:0a0b0c0d");
        assert_eq!(
            env_pair("sk-ant-oat01-fake"),
            (
                "CLAUDE_CODE_OAUTH_TOKEN".to_owned(),
                "sk-ant-oat01-fake".to_owned()
            )
        );
    }

    #[test]
    fn a_pasted_token_is_trimmed_and_an_unusable_one_is_refused() {
        assert_eq!(
            clean_token("  sk-ant-oat01-fake\n").as_deref(),
            Some("sk-ant-oat01-fake")
        );
        for bad in ["", "   ", "\n\t", "sk-ant oat", "sk-ant\u{7}x"] {
            assert_eq!(clean_token(bad), None, "{bad:?}");
        }
    }

    #[tokio::test]
    async fn store_load_delete_round_trip() {
        let id = AccountId("claude:token-unit".into());
        assert_eq!(load(&id).await.unwrap(), None);
        store(&id, "sk-ant-oat01-fake").await.unwrap();
        assert_eq!(
            load(&id).await.unwrap().as_deref(),
            Some("sk-ant-oat01-fake")
        );
        delete(&id).await.unwrap();
        assert_eq!(load(&id).await.unwrap(), None);
        delete(&id).await.unwrap();
    }
}
