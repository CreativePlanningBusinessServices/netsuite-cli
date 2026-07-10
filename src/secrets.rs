use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::error::CliError;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum AccountSecrets {
    M2m {
        client_id: String,
        cert_id: String,
        private_key_pem: String,
    },
    AuthCode {
        client_id: String,
        refresh_token: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedToken {
    pub access_token: String,
    pub expires_at_epoch: u64,
}

impl CachedToken {
    const LEEWAY_SECONDS: u64 = 60;

    pub fn is_valid_at(&self, now_epoch: u64) -> bool {
        self.expires_at_epoch > now_epoch + Self::LEEWAY_SECONDS
    }
}

pub trait SecretStore: Send + Sync {
    fn get(&self, alias: &str) -> Result<Option<AccountSecrets>, CliError>;
    fn set(&self, alias: &str, secrets: &AccountSecrets) -> Result<(), CliError>;
    fn delete(&self, alias: &str) -> Result<(), CliError>;
    fn get_token(&self, alias: &str) -> Result<Option<CachedToken>, CliError>;
    fn set_token(&self, alias: &str, token: &CachedToken) -> Result<(), CliError>;
    fn delete_token(&self, alias: &str) -> Result<(), CliError>;
}

pub struct KeyringStore;

const KEYRING_SERVICE: &str = "netsuite-cli";

impl KeyringStore {
    fn entry(user: &str) -> Result<keyring::Entry, CliError> {
        keyring::Entry::new(KEYRING_SERVICE, user).map_err(|keyring_error| {
            CliError::Auth(format!("keychain unavailable: {keyring_error}"))
        })
    }

    fn read<T: for<'de> Deserialize<'de>>(user: &str) -> Result<Option<T>, CliError> {
        match Self::entry(user)?.get_password() {
            Ok(raw) => serde_json::from_str(&raw).map(Some).map_err(|parse_error| {
                CliError::Auth(format!("corrupt keychain entry '{user}': {parse_error}"))
            }),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(keyring_error) => Err(CliError::Auth(format!(
                "keychain read failed: {keyring_error}"
            ))),
        }
    }

    fn write<T: Serialize>(user: &str, value: &T) -> Result<(), CliError> {
        Self::entry(user)?
            .set_password(&serde_json::to_string(value).expect("serializable"))
            .map_err(|keyring_error| {
                CliError::Auth(format!("keychain write failed: {keyring_error}"))
            })
    }

    fn remove(user: &str) -> Result<(), CliError> {
        match Self::entry(user)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(keyring_error) => Err(CliError::Auth(format!(
                "keychain delete failed: {keyring_error}"
            ))),
        }
    }
}

impl SecretStore for KeyringStore {
    fn get(&self, alias: &str) -> Result<Option<AccountSecrets>, CliError> {
        KeyringStore::read(alias)
    }
    fn set(&self, alias: &str, secrets: &AccountSecrets) -> Result<(), CliError> {
        KeyringStore::write(alias, secrets)
    }
    fn delete(&self, alias: &str) -> Result<(), CliError> {
        KeyringStore::remove(alias)?;
        KeyringStore::remove(&format!("{alias}#token"))
    }
    fn get_token(&self, alias: &str) -> Result<Option<CachedToken>, CliError> {
        KeyringStore::read(&format!("{alias}#token"))
    }
    fn set_token(&self, alias: &str, token: &CachedToken) -> Result<(), CliError> {
        KeyringStore::write(&format!("{alias}#token"), token)
    }
    fn delete_token(&self, alias: &str) -> Result<(), CliError> {
        KeyringStore::remove(&format!("{alias}#token"))
    }
}

#[derive(Default)]
pub struct MemoryStore {
    entries: Mutex<HashMap<String, String>>,
}

impl MemoryStore {
    fn read<T: for<'de> Deserialize<'de>>(&self, key: &str) -> Result<Option<T>, CliError> {
        Ok(self
            .entries
            .lock()
            .unwrap()
            .get(key)
            .map(|raw| serde_json::from_str(raw).expect("valid stored json")))
    }

    fn write<T: Serialize>(&self, key: &str, value: &T) -> Result<(), CliError> {
        self.entries
            .lock()
            .unwrap()
            .insert(key.into(), serde_json::to_string(value).unwrap());
        Ok(())
    }
}

impl SecretStore for MemoryStore {
    fn get(&self, alias: &str) -> Result<Option<AccountSecrets>, CliError> {
        self.read(alias)
    }
    fn set(&self, alias: &str, secrets: &AccountSecrets) -> Result<(), CliError> {
        self.write(alias, secrets)
    }
    fn delete(&self, alias: &str) -> Result<(), CliError> {
        let mut entries = self.entries.lock().unwrap();
        entries.remove(alias);
        entries.remove(&format!("{alias}#token"));
        Ok(())
    }
    fn get_token(&self, alias: &str) -> Result<Option<CachedToken>, CliError> {
        self.read(&format!("{alias}#token"))
    }
    fn set_token(&self, alias: &str, token: &CachedToken) -> Result<(), CliError> {
        self.write(&format!("{alias}#token"), token)
    }
    fn delete_token(&self, alias: &str) -> Result<(), CliError> {
        self.entries
            .lock()
            .unwrap()
            .remove(&format!("{alias}#token"));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trips_secrets_and_tokens() {
        let store = MemoryStore::default();
        let secrets = AccountSecrets::M2m {
            client_id: "cid".into(),
            cert_id: "kid123".into(),
            private_key_pem: "PEM".into(),
        };
        store.set("prod", &secrets).unwrap();
        match store.get("prod").unwrap().expect("stored") {
            AccountSecrets::M2m { cert_id, .. } => assert_eq!(cert_id, "kid123"),
            other => panic!("wrong variant: {other:?}"),
        }
        assert!(store.get("absent").unwrap().is_none());

        let token = CachedToken {
            access_token: "tok".into(),
            expires_at_epoch: 999,
        };
        store.set_token("prod", &token).unwrap();
        assert_eq!(
            store.get_token("prod").unwrap().unwrap().access_token,
            "tok"
        );
        store.delete("prod").unwrap();
        assert!(store.get("prod").unwrap().is_none());
    }

    #[test]
    fn cached_token_expiry_check_uses_leeway() {
        let now = 1_000_000;
        let live = CachedToken {
            access_token: "a".into(),
            expires_at_epoch: now + 120,
        };
        let stale = CachedToken {
            access_token: "b".into(),
            expires_at_epoch: now + 10,
        };
        assert!(live.is_valid_at(now));
        assert!(!stale.is_valid_at(now)); // 60s leeway
    }
}
