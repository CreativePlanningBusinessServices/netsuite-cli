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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TbaSecrets {
    pub consumer_key: String,
    pub consumer_secret: String,
    pub token_id: Option<String>,
    pub token_secret: Option<String>,
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
    fn get_tba(&self, alias: &str) -> Result<Option<TbaSecrets>, CliError>;
    fn set_tba(&self, alias: &str, secrets: &TbaSecrets) -> Result<(), CliError>;
    fn delete_tba(&self, alias: &str) -> Result<(), CliError>;
}

pub struct KeyringStore;

const KEYRING_SERVICE: &str = "netsuite-cli";

/// Windows Credential Manager caps one credential's blob at CRED_MAX_CREDENTIAL_BLOB_SIZE
/// (2560) bytes, and the keyring crate stores the password UTF-16 encoded, so a single entry
/// holds at most 1280 UTF-16 code units. NetSuite auth-code payloads blow through that — a
/// refresh token alone can exceed it — so payloads over this budget are split across
/// `<user>#chunk<i>` entries, with the main entry holding a marker that records the chunk
/// count. The threshold leaves headroom under the ceiling and applies on every platform
/// (macOS has no such limit) so stored layouts do not diverge per OS.
const MAX_ENTRY_UTF16_UNITS: usize = 1024;

/// What the main entry holds when the payload is chunked. `deny_unknown_fields` plus the
/// distinctive field name guarantee no real payload (`AccountSecrets`, `CachedToken`,
/// `TbaSecrets` — all carry other required fields) can ever parse as a marker.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChunkMarker {
    #[serde(rename = "netsuite-cli-chunk-count")]
    chunk_count: usize,
}

fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}

/// Splits on char boundaries, packing each chunk with as many chars as fit in `max_units`
/// UTF-16 code units. Concatenating the chunks in order reproduces the input exactly.
fn split_utf16_chunks(payload: &str, max_units: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_units = 0;
    for character in payload.chars() {
        let character_units = character.len_utf16();
        if current_units + character_units > max_units {
            chunks.push(std::mem::take(&mut current));
            current_units = 0;
        }
        current.push(character);
        current_units += character_units;
    }
    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

impl KeyringStore {
    fn entry(user: &str) -> Result<keyring::Entry, CliError> {
        keyring::Entry::new(KEYRING_SERVICE, user).map_err(|keyring_error| {
            CliError::Auth(format!("keychain unavailable: {keyring_error}"))
        })
    }

    fn read<T: for<'de> Deserialize<'de>>(user: &str) -> Result<Option<T>, CliError> {
        let raw = match Self::entry(user)?.get_password() {
            Ok(raw) => raw,
            Err(keyring::Error::NoEntry) => return Ok(None),
            Err(keyring_error) => {
                return Err(CliError::Auth(format!(
                    "keychain read failed: {keyring_error}"
                )));
            }
        };
        let payload = match serde_json::from_str::<ChunkMarker>(&raw) {
            Ok(marker) => Self::read_chunks(user, marker.chunk_count)?,
            Err(_) => raw,
        };
        serde_json::from_str(&payload)
            .map(Some)
            .map_err(|parse_error| {
                CliError::Auth(format!("corrupt keychain entry '{user}': {parse_error}"))
            })
    }

    fn read_chunks(user: &str, chunk_count: usize) -> Result<String, CliError> {
        let mut payload = String::new();
        for chunk_index in 0..chunk_count {
            let chunk_user = format!("{user}#chunk{chunk_index}");
            match Self::entry(&chunk_user)?.get_password() {
                Ok(chunk) => payload.push_str(&chunk),
                Err(keyring_error) => {
                    return Err(CliError::Auth(format!(
                        "keychain read failed for chunk '{chunk_user}': {keyring_error}"
                    )));
                }
            }
        }
        Ok(payload)
    }

    fn write<T: Serialize>(user: &str, value: &T) -> Result<(), CliError> {
        let payload = serde_json::to_string(value).expect("serializable");
        if utf16_len(&payload) <= MAX_ENTRY_UTF16_UNITS {
            Self::write_entry(user, &payload)?;
            Self::remove_chunks_from(user, 0)
        } else {
            let chunks = split_utf16_chunks(&payload, MAX_ENTRY_UTF16_UNITS);
            let marker = serde_json::to_string(&ChunkMarker {
                chunk_count: chunks.len(),
            })
            .expect("serializable");
            // Chunks land before the marker so a write that dies partway leaves the main
            // entry pointing at whatever was stored before, never at missing chunks.
            for (chunk_index, chunk) in chunks.iter().enumerate() {
                Self::write_entry(&format!("{user}#chunk{chunk_index}"), chunk)?;
            }
            Self::write_entry(user, &marker)?;
            Self::remove_chunks_from(user, chunks.len())
        }
    }

    fn write_entry(user: &str, payload: &str) -> Result<(), CliError> {
        Self::entry(user)?
            .set_password(payload)
            .map_err(|keyring_error| {
                CliError::Auth(format!("keychain write failed: {keyring_error}"))
            })
    }

    /// Deletes chunk entries from `first_chunk_index` up to the first missing one — the
    /// stale tail left behind when a value shrinks or stops being chunked.
    fn remove_chunks_from(user: &str, first_chunk_index: usize) -> Result<(), CliError> {
        let mut chunk_index = first_chunk_index;
        loop {
            let chunk_user = format!("{user}#chunk{chunk_index}");
            match Self::entry(&chunk_user)?.delete_credential() {
                Ok(()) => chunk_index += 1,
                Err(keyring::Error::NoEntry) => return Ok(()),
                Err(keyring_error) => {
                    return Err(CliError::Auth(format!(
                        "keychain delete failed: {keyring_error}"
                    )));
                }
            }
        }
    }

    fn remove(user: &str) -> Result<(), CliError> {
        match Self::entry(user)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Self::remove_chunks_from(user, 0),
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
        KeyringStore::remove(&format!("{alias}#token"))?;
        KeyringStore::remove(&format!("{alias}#tba"))
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
    fn get_tba(&self, alias: &str) -> Result<Option<TbaSecrets>, CliError> {
        KeyringStore::read(&format!("{alias}#tba"))
    }
    fn set_tba(&self, alias: &str, secrets: &TbaSecrets) -> Result<(), CliError> {
        KeyringStore::write(&format!("{alias}#tba"), secrets)
    }
    fn delete_tba(&self, alias: &str) -> Result<(), CliError> {
        KeyringStore::remove(&format!("{alias}#tba"))
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
        entries.remove(&format!("{alias}#tba"));
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
    fn get_tba(&self, alias: &str) -> Result<Option<TbaSecrets>, CliError> {
        self.read(&format!("{alias}#tba"))
    }
    fn set_tba(&self, alias: &str, secrets: &TbaSecrets) -> Result<(), CliError> {
        self.write(&format!("{alias}#tba"), secrets)
    }
    fn delete_tba(&self, alias: &str) -> Result<(), CliError> {
        self.entries.lock().unwrap().remove(&format!("{alias}#tba"));
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

    #[test]
    fn split_utf16_chunks_round_trips_and_respects_the_unit_budget() {
        let payload = "x".repeat(2500);
        let chunks = split_utf16_chunks(&payload, 1024);
        assert_eq!(chunks.len(), 3);
        assert!(chunks.iter().all(|chunk| utf16_len(chunk) <= 1024));
        assert_eq!(chunks.concat(), payload);

        // exactly at the budget stays a single chunk
        let exact = "y".repeat(1024);
        assert_eq!(split_utf16_chunks(&exact, 1024), vec![exact.clone()]);
    }

    #[test]
    fn split_utf16_chunks_counts_utf16_units_not_chars() {
        // '🔑' is one char but two UTF-16 code units; a budget of 3 units fits one
        // emoji plus one ASCII char, never two emoji.
        let payload = "🔑a🔑b";
        let chunks = split_utf16_chunks(payload, 3);
        assert_eq!(chunks, vec!["🔑a".to_string(), "🔑b".to_string()]);
        assert!(chunks.iter().all(|chunk| utf16_len(chunk) <= 3));
        assert_eq!(chunks.concat(), payload);
    }

    #[test]
    fn chunk_marker_is_never_confused_with_real_payloads() {
        // no stored payload type parses as a marker …
        let auth_code = serde_json::to_string(&AccountSecrets::AuthCode {
            client_id: "cid".into(),
            refresh_token: Some("refresh".into()),
        })
        .unwrap();
        let token = serde_json::to_string(&CachedToken {
            access_token: "tok".into(),
            expires_at_epoch: 999,
        })
        .unwrap();
        let tba = serde_json::to_string(&TbaSecrets {
            consumer_key: "k".into(),
            consumer_secret: "s".into(),
            token_id: None,
            token_secret: None,
        })
        .unwrap();
        for raw in [&auth_code, &token, &tba] {
            assert!(serde_json::from_str::<ChunkMarker>(raw).is_err());
        }

        // … and a marker parses as nothing but a marker
        let marker = serde_json::to_string(&ChunkMarker { chunk_count: 3 }).unwrap();
        assert!(serde_json::from_str::<AccountSecrets>(&marker).is_err());
        assert!(serde_json::from_str::<CachedToken>(&marker).is_err());
        assert!(serde_json::from_str::<TbaSecrets>(&marker).is_err());
        assert_eq!(
            serde_json::from_str::<ChunkMarker>(&marker)
                .unwrap()
                .chunk_count,
            3
        );
    }

    #[test]
    fn oversized_auth_code_secrets_split_and_reassemble() {
        // a NetSuite-sized refresh token JWT comfortably exceeds one entry's budget
        let secrets = AccountSecrets::AuthCode {
            client_id: "c".repeat(64),
            refresh_token: Some("r".repeat(3000)),
        };
        let payload = serde_json::to_string(&secrets).unwrap();
        assert!(utf16_len(&payload) > MAX_ENTRY_UTF16_UNITS);

        let chunks = split_utf16_chunks(&payload, MAX_ENTRY_UTF16_UNITS);
        assert!(chunks.len() > 1);
        match serde_json::from_str::<AccountSecrets>(&chunks.concat()).unwrap() {
            AccountSecrets::AuthCode { refresh_token, .. } => {
                assert_eq!(refresh_token.unwrap().len(), 3000);
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn tba_secrets_round_trip_and_are_removed_with_the_account() {
        let store = MemoryStore::default();
        assert!(store.get_tba("demo").unwrap().is_none());

        let tba = TbaSecrets {
            consumer_key: "consumerkey123".into(),
            consumer_secret: "consumersecret789".into(),
            token_id: None,
            token_secret: None,
        };
        store.set_tba("demo", &tba).unwrap();
        let stored = store.get_tba("demo").unwrap().expect("stored");
        assert_eq!(stored.consumer_key, "consumerkey123");
        assert!(stored.token_id.is_none());

        let minted = TbaSecrets {
            token_id: Some("tokenid456".into()),
            token_secret: Some("tokensecret012".into()),
            ..tba
        };
        store.set_tba("demo", &minted).unwrap();
        assert_eq!(
            store.get_tba("demo").unwrap().unwrap().token_id.as_deref(),
            Some("tokenid456")
        );

        // account removal must sweep the TBA entry too
        store.delete("demo").unwrap();
        assert!(store.get_tba("demo").unwrap().is_none());

        store
            .set_tba(
                "demo",
                &TbaSecrets {
                    consumer_key: "k".into(),
                    consumer_secret: "s".into(),
                    token_id: None,
                    token_secret: None,
                },
            )
            .unwrap();
        store.delete_tba("demo").unwrap();
        assert!(store.get_tba("demo").unwrap().is_none());
    }
}
