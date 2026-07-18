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
