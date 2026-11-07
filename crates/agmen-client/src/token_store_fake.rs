//! Scoped in-memory backends for the real keychain entry path in unit tests.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};

use super::*;

static STORES: LazyLock<Mutex<HashMap<TokenKey, Arc<CredentialStore>>>> =
    LazyLock::new(Mutex::default);

pub(crate) struct FakeKeychain(TokenKey);

impl FakeKeychain {
    pub(crate) fn new(key: TokenKey) -> Self {
        let store = keyring_core::mock::Store::new().unwrap();
        assert!(STORES.lock().unwrap().insert(key.clone(), store).is_none());
        Self(key)
    }
}

impl Drop for FakeKeychain {
    fn drop(&mut self) {
        STORES.lock().unwrap().remove(&self.0);
    }
}

pub(super) fn store(key: &TokenKey) -> Option<Arc<CredentialStore>> {
    STORES.lock().unwrap().get(key).cloned()
}
