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

pub fn delete_secret(key: &str) -> keyring::Result<()> {
    match Entry::new(SERVICE_NAME, key)?.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e),
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
