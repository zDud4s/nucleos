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
}
