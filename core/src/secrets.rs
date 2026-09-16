//! §spec modulo-de-github

use keyring::Entry;

const SERVICE_NAME: &str = "nucleos";

pub fn store_secret(key: &str, value: &str) -> keyring::Result<()> {
    Entry::new(SERVICE_NAME, key)?.set_password(value)
}

pub fn load_secret(key: &str) -> keyring::Result<Option<String>> {
    match Entry::new(SERVICE_NAME, key)?.get_password() {
        Ok(v) => Ok(Some(v)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e),
    }
}

/// The third of the store/load/delete trio. Nothing in the daemon removes a credential yet — token
/// rotation overwrites — but the roundtrip test needs it to clean up after itself honestly, and a
/// credential wrapper that cannot delete is an abstraction with a hole in it rather than a small one.
#[allow(dead_code)]
pub fn delete_secret(key: &str) -> keyring::Result<()> {
    match Entry::new(SERVICE_NAME, key)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Why the daemon will not start when the system credential store is out of reach.
///
/// Said per platform because the fix is per platform. There is deliberately no file fallback: the
/// daemon's token authorises everything, and a token in a `0600` file is a different security
/// decision that deserves to be taken on its own (portability spec, D4).
pub fn unavailable_message(error: &str) -> String {
    #[cfg(windows)]
    let missing = "Windows Credential Manager could not be used";
    #[cfg(target_os = "macos")]
    let missing = "the macOS login Keychain could not be used (is it locked?)";
    #[cfg(not(any(windows, target_os = "macos")))]
    let missing = "no Secret Service is reachable over D-Bus: \
                   start gnome-keyring or KWallet in this session and unlock it";
    format!(
        "nucleos-core cannot start: {missing} ({error}). The daemon keeps its token in the system \
         credential store and has no file fallback."
    )
}

/// The daemon's own token at startup: the stored one, or a fresh one that was stored, or the
/// sentence to refuse with.
///
/// PURE over its inputs so every branch is testable without a credential store: `loaded` is what
/// [`load_secret`] answered, `mint` makes a token, `persist` stores it. An `Err` is printed and
/// the daemon exits non-zero -- a refusal, never a panic (portability spec, D4).
pub fn daemon_token<E: std::fmt::Display>(
    loaded: Result<Option<String>, E>,
    mint: impl FnOnce() -> String,
    persist: impl FnOnce(&str) -> Result<(), E>,
) -> Result<String, String> {
    match loaded {
        Ok(Some(existing)) => Ok(existing),
        Ok(None) => {
            let fresh = mint();
            persist(&fresh).map_err(|error| unavailable_message(&error.to_string()))?;
            Ok(fresh)
        }
        Err(error) => Err(unavailable_message(&error.to_string())),
    }
}

/// The credential store, as something a test can stand in for.
///
/// The free functions above talk to the real Windows Credential Manager and there is no fake
/// backend for `keyring` — which is why the roundtrip test below is `#[ignore]`d. That was fine
/// while nothing but startup read a secret. It stops being fine the moment a ROUTE writes one:
/// a route with no test is a route whose refusals are asserted by nobody, and the refusals are
/// the whole of this one's contract.
///
/// So the same shape `AppState` already uses for `runner` and `assistants` — a trait object,
/// production wiring gets the real thing, tests get a double. The trait is deliberately narrow:
/// three methods, and **no method that returns a secret**. See [`SecretStore::present`].
pub trait SecretStore: Send + Sync {
    /// Whether a credential is stored, and NEVER what it is.
    ///
    /// The one design decision in this file worth arguing about, so: nothing outside `main.rs`
    /// startup needs to READ a secret, and a trait that could return one would put every future
    /// caller one keystroke away from serving it. The API answers "is it set", which is the only
    /// question a settings page has.
    fn present(&self, key: &str) -> Result<bool, String>;
    fn store(&self, key: &str, value: &str) -> Result<(), String>;
    fn forget(&self, key: &str) -> Result<(), String>;
}

/// The real one: the OS credential manager, through the free functions above.
pub struct OsCredentialStore;

impl SecretStore for OsCredentialStore {
    fn present(&self, key: &str) -> Result<bool, String> {
        load_secret(key)
            .map(|value| value.is_some())
            .map_err(|error| error.to_string())
    }

    fn store(&self, key: &str, value: &str) -> Result<(), String> {
        store_secret(key, value).map_err(|error| error.to_string())
    }

    fn forget(&self, key: &str) -> Result<(), String> {
        delete_secret(key).map_err(|error| error.to_string())
    }
}

/// A store in a `Mutex<HashMap>`, for the tests that must not touch a real desktop's credentials.
///
/// `#[cfg(test)]` and not a feature flag: production has exactly one store and there is no
/// configuration under which it should have another.
#[cfg(test)]
#[derive(Default)]
pub struct InMemorySecrets {
    entries: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

#[cfg(test)]
impl SecretStore for InMemorySecrets {
    fn present(&self, key: &str) -> Result<bool, String> {
        Ok(self.entries.lock().unwrap().contains_key(key))
    }

    fn store(&self, key: &str, value: &str) -> Result<(), String> {
        self.entries
            .lock()
            .unwrap()
            .insert(key.to_owned(), value.to_owned());
        Ok(())
    }

    fn forget(&self, key: &str) -> Result<(), String> {
        self.entries.lock().unwrap().remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // This test touches the real Windows Credential Manager — there is no fake/mock backend for
    // `keyring`, and spec §5 chose the real OS store deliberately, so testing against a stub would
    // not actually verify the integration works. Uses a namespaced test key and cleans up both
    // before and after to tolerate a prior failed run leaving state behind.
    //
    // #[ignore] keeps this out of the default `cargo test` run (the ongoing gate used by every other
    // task in this plan, which needs to stay runnable headlessly) — run it explicitly with
    // `cargo test -- --include-ignored` on a real desktop session where Credential Manager is
    // actually reachable.
    #[test]
    #[ignore = "touches the real Windows Credential Manager; run with --include-ignored on a desktop session"]
    fn store_load_delete_roundtrip() {
        let key = "test-roundtrip-secret";
        let _ = delete_secret(key);

        store_secret(key, "super-secret-value").unwrap();
        assert_eq!(
            load_secret(key).unwrap(),
            Some("super-secret-value".to_string())
        );

        delete_secret(key).unwrap();
        assert_eq!(load_secret(key).unwrap(), None);
    }

    /// D4: the sentence a daemon prints before refusing to start. It carries the store's own error
    /// and names what is missing on THIS platform, because the fix differs per platform.
    #[test]
    fn the_refusal_to_start_names_what_is_missing_on_this_platform() {
        let said = unavailable_message("the probe error");
        assert!(said.starts_with("nucleos-core cannot start: "), "{said}");
        assert!(said.contains("the probe error"), "{said}");
        assert!(said.contains("no file fallback"), "{said}");
        let names = if cfg!(windows) {
            vec!["Credential Manager"]
        } else if cfg!(target_os = "macos") {
            vec!["Keychain"]
        } else {
            vec!["Secret Service", "gnome-keyring"]
        };
        for name in names {
            assert!(said.contains(name), "missing {name:?} in {said}");
        }
    }

    #[test]
    fn a_stored_daemon_token_is_used_and_nothing_is_minted_or_persisted() {
        let token = daemon_token(
            Ok::<_, String>(Some("stored".to_owned())),
            || -> String { panic!("minted") },
            |_: &str| -> Result<(), String> { panic!("persisted") },
        );
        assert_eq!(token, Ok("stored".to_owned()));
    }

    #[test]
    fn no_stored_daemon_token_mints_one_and_persists_it() {
        let mut persisted = None;
        let token = daemon_token(
            Ok::<_, String>(None),
            || "fresh".to_owned(),
            |fresh: &str| {
                persisted = Some(fresh.to_owned());
                Ok(())
            },
        );
        assert_eq!(token, Ok("fresh".to_owned()));
        assert_eq!(persisted.as_deref(), Some("fresh"));
    }

    #[test]
    fn a_store_that_cannot_be_read_refuses_to_start_without_minting() {
        let refused = daemon_token(
            Err::<Option<String>, _>("no D-Bus session".to_owned()),
            || -> String { panic!("minted") },
            |_: &str| -> Result<(), String> { panic!("persisted") },
        )
        .unwrap_err();
        assert_eq!(refused, unavailable_message("no D-Bus session"));
    }

    #[test]
    fn a_token_that_cannot_be_persisted_refuses_to_start() {
        let refused = daemon_token(
            Ok::<_, String>(None),
            || "fresh".to_owned(),
            |_: &str| Err("store is locked".to_owned()),
        )
        .unwrap_err();
        assert_eq!(refused, unavailable_message("store is locked"));
    }
}
