//! Credential storage in the OS keyring.
//!
//! Access and refresh tokens must not end up in `config.json`: that file is
//! plain text, is copied into bug reports, and often lands in a synced folder.
//! The keyring maps to DPAPI on Windows, Secret Service on Linux, and the
//! Keychain on macOS, all of which are encrypted at rest under the user's login.
//!
//! One credential is not big enough. Windows Credential Manager caps a password at
//! 2560 characters, and a Microsoft refresh token alone can approach 2000, so
//! storing three tokens as one JSON blob fails outright. Each token therefore gets
//! its own entries, split into chunks when a single token is still too long.

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::error::{MangoError, Result};

/// Service name used for every entry ("MangoLauncher").
const SERVICE: &str = "mangolauncher";

/// Characters per stored chunk.
///
/// Windows reports "password encoded as UTF-16 is longer than platform limit of
/// 2560 chars", and the limit counts the encoded form, so the usable width is
/// lower than 2560. Half of it leaves room for that encoding and for the platform's
/// own overhead, which a real token run confirmed is needed.
const CHUNK_CHARS: usize = 1000;

/// Secrets belonging to one account.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredSecrets {
    /// Microsoft access token, short lived.
    pub msa_access_token: String,
    /// Microsoft refresh token, long lived. This is the sensitive one.
    pub msa_refresh_token: String,
    /// Minecraft access token, handed to the game.
    pub mc_access_token: String,
}

impl StoredSecrets {
    /// Never print the tokens, even accidentally through `Debug`.
    fn redacted(&self) -> String {
        let mask = |value: &str| {
            if value.len() > 8 {
                format!("…{}", &value[value.len() - 4..])
            } else {
                "…".to_owned()
            }
        };
        format!(
            "StoredSecrets {{ msa_access_token: {}, msa_refresh_token: {}, mc_access_token: {} }}",
            mask(&self.msa_access_token),
            mask(&self.msa_refresh_token),
            mask(&self.mc_access_token)
        )
    }

    /// The three fields, each with the key suffix used in the keyring.
    fn fields(&self) -> [(&'static str, &str); 3] {
        [
            ("msa_access", &self.msa_access_token),
            ("msa_refresh", &self.msa_refresh_token),
            ("mc_access", &self.mc_access_token),
        ]
    }
}

impl std::fmt::Display for StoredSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.redacted())
    }
}

/// Split a value into chunks the platform will accept.
///
/// An empty value yields no chunks: nothing to store, and a missing entry is read
/// back as an empty string.
fn split_chunks(value: &str) -> Vec<String> {
    if value.is_empty() {
        return Vec::new();
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut width = 0;

    for character in value.chars() {
        current.push(character);
        width += 1;
        if width >= CHUNK_CHARS {
            chunks.push(std::mem::take(&mut current));
            width = 0;
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Keyring username for one chunk of one field.
fn key(uuid: &str, field: &str, index: usize) -> String {
    format!("{uuid}:{field}:{index}")
}

/// Keyring username used by the older single-blob format.
fn legacy_key(uuid: &str) -> String {
    uuid.to_owned()
}

fn entry(key: &str) -> Result<keyring::Entry> {
    keyring::Entry::new(SERVICE, key)
        .map_err(|err| MangoError::Other(format!("keyring is unavailable: {err}")))
}

fn write_chunk(key: &str, value: &str) -> Result<()> {
    entry(key)?.set_password(value).map_err(|err| {
        MangoError::Other(format!("could not save credentials to the keyring: {err}"))
    })
}

fn read_chunk(key: &str) -> Result<Option<String>> {
    match entry(key)?.get_password() {
        Ok(value) => Ok(Some(value)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => {
            Err(MangoError::Other(format!("could not read credentials from the keyring: {err}")))
        }
    }
}

fn remove_entry(key: &str) -> Result<()> {
    match entry(key)?.delete_credential() {
        Ok(()) => Ok(()),
        // Deleting something that is not there is not a failure.
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => {
            Err(MangoError::Other(format!("could not remove credentials from the keyring: {err}")))
        }
    }
}

/// Write a value as chunks, removing any chunks left over from a longer value.
fn write_field(uuid: &str, field: &str, value: &str) -> Result<()> {
    let chunks = split_chunks(value);

    for (index, chunk) in chunks.iter().enumerate() {
        write_chunk(&key(uuid, field, index), chunk)?;
    }

    // A shortened token must not leave a stale tail behind, which would be read
    // back as part of the value.
    let mut index = chunks.len();
    while read_chunk(&key(uuid, field, index))?.is_some() {
        remove_entry(&key(uuid, field, index))?;
        index += 1;
    }

    Ok(())
}

/// Read a value written by [`write_field`].
fn read_field(uuid: &str, field: &str) -> Result<String> {
    let mut value = String::new();
    let mut index = 0;

    while let Some(chunk) = read_chunk(&key(uuid, field, index))? {
        value.push_str(&chunk);
        index += 1;
    }

    Ok(value)
}

/// Remove every chunk of a field, however many there are.
fn clear_field(uuid: &str, field: &str) -> Result<()> {
    let mut index = 0;
    while read_chunk(&key(uuid, field, index))?.is_some() {
        remove_entry(&key(uuid, field, index))?;
        index += 1;
    }
    Ok(())
}

/// Save an account's secrets, replacing anything already stored for that uuid.
pub fn save(uuid: &str, secrets: &StoredSecrets) -> Result<()> {
    // The older format stored one JSON blob under the bare uuid; leaving it would
    // keep a second, stale copy of the tokens around.
    remove_entry(&legacy_key(uuid))?;

    for (field, value) in secrets.fields() {
        write_field(uuid, field, value)?;
    }

    debug!(uuid, "stored credentials in the keyring");
    Ok(())
}

/// Load an account's secrets, or `None` when nothing is stored.
pub fn load(uuid: &str) -> Result<Option<StoredSecrets>> {
    let msa_access_token = read_field(uuid, "msa_access")?;
    let msa_refresh_token = read_field(uuid, "msa_refresh")?;
    let mc_access_token = read_field(uuid, "mc_access")?;

    if !msa_access_token.is_empty() || !msa_refresh_token.is_empty() || !mc_access_token.is_empty()
    {
        return Ok(Some(StoredSecrets { msa_access_token, msa_refresh_token, mc_access_token }));
    }

    // Nothing in the new layout: try the single-blob format written by older
    // builds, and migrate it so the next save uses the chunked layout.
    let Some(payload) = read_chunk(&legacy_key(uuid))? else {
        return Ok(None);
    };

    match serde_json::from_str::<StoredSecrets>(&payload) {
        Ok(secrets) => {
            debug!(uuid, "migrating a single-blob keyring entry");
            save(uuid, &secrets)?;
            Ok(Some(secrets))
        }
        Err(err) => {
            // A malformed entry is worse than none: report it as missing so the
            // player can sign in again instead of hitting a parse error.
            warn!(uuid, error = %err, "discarding an unreadable keyring entry");
            remove_entry(&legacy_key(uuid))?;
            Ok(None)
        }
    }
}

/// Remove an account's secrets.
pub fn delete(uuid: &str) -> Result<()> {
    for field in ["msa_access", "msa_refresh", "mc_access"] {
        clear_field(uuid, field)?;
    }
    remove_entry(&legacy_key(uuid))?;

    debug!(uuid, "removed credentials from the keyring");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> StoredSecrets {
        StoredSecrets {
            msa_access_token: "msa-access-token-value".to_owned(),
            msa_refresh_token: "msa-refresh-token-value".to_owned(),
            mc_access_token: "mc-access-token-value".to_owned(),
        }
    }

    #[test]
    fn secrets_round_trip_through_json() {
        let secrets = sample();
        let text = serde_json::to_string(&secrets).unwrap();
        let parsed: StoredSecrets = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed, secrets);
    }

    #[test]
    fn debug_output_does_not_leak_tokens() {
        // The whole point: a token in a log line is a leaked token.
        let text = sample().redacted();
        assert!(!text.contains("msa-access-token-value"));
        assert!(!text.contains("msa-refresh-token-value"));
        assert!(!text.contains("mc-access-token-value"));
        assert!(text.contains("StoredSecrets"));
    }

    #[test]
    fn a_short_value_is_one_chunk() {
        assert_eq!(split_chunks("abc"), vec!["abc".to_owned()]);
    }

    #[test]
    fn an_empty_value_is_no_chunks() {
        // Storing nothing is how an absent field is represented; a stored empty
        // string would make "missing" and "empty" indistinguishable.
        assert!(split_chunks("").is_empty());
    }

    #[test]
    fn a_value_longer_than_the_limit_is_split() {
        // A real Microsoft refresh token is around this size, and one credential
        // holds 2560 characters of UTF-16 on Windows.
        let value = "t".repeat(5000);
        let chunks = split_chunks(&value);

        assert_eq!(chunks.len(), 5, "5000 chars at {CHUNK_CHARS} per chunk");
        for chunk in &chunks {
            assert!(chunk.chars().count() <= CHUNK_CHARS, "a chunk must fit one credential");
        }
        assert_eq!(chunks.concat(), value, "splitting must be lossless");
    }

    #[test]
    fn joining_the_chunks_restores_the_value() {
        let value = format!("{}{}", "a".repeat(3000), "z".repeat(2500));
        let chunks = split_chunks(&value);
        assert_eq!(chunks.concat(), value);
    }

    #[test]
    fn a_value_exactly_at_the_limit_is_one_chunk() {
        let value = "x".repeat(CHUNK_CHARS);
        assert_eq!(split_chunks(&value).len(), 1);
    }

    #[test]
    fn multi_byte_characters_are_not_split_in_the_middle() {
        // A token is ASCII, but a surrogate pair or a multi-byte character must not
        // be cut: the platform counts characters, and a broken encoding would be
        // rejected.
        let value = "é".repeat(CHUNK_CHARS + 10);
        let chunks = split_chunks(&value);
        assert_eq!(chunks.concat(), value);
        assert_eq!(chunks[0].chars().count(), CHUNK_CHARS);
    }

    #[test]
    fn the_chunk_keys_are_distinct_per_field() {
        // One credential per chunk: reusing a key would overwrite the previous value.
        assert_eq!(key("uuid", "msa_refresh", 0), "uuid:msa_refresh:0");
        assert_eq!(key("uuid", "msa_refresh", 1), "uuid:msa_refresh:1");
        assert_ne!(key("uuid", "msa_refresh", 0), key("uuid", "mc_access", 0));
    }

    #[test]
    fn the_legacy_key_is_the_bare_uuid() {
        // The old format keyed on the uuid alone, so migrating has to look there.
        assert_eq!(legacy_key("some-uuid"), "some-uuid");
    }

    #[test]
    fn a_field_list_covers_every_token() {
        // A token missing from this list would silently never be persisted.
        let secrets = sample();
        let fields = secrets.fields();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0], ("msa_access", "msa-access-token-value"));
        assert_eq!(fields[1], ("msa_refresh", "msa-refresh-token-value"));
        assert_eq!(fields[2], ("mc_access", "mc-access-token-value"));
    }

    /// Round-trip through the real OS keyring, with a token the size that broke it.
    ///
    /// The failure this guards against was a platform limit, so only the real
    /// backend can prove the fix. It is ignored by default because CI runners have
    /// no logged-in user to unlock a keyring; run it with
    /// `cargo test -- --ignored keyring`.
    #[test]
    #[ignore = "needs an unlocked OS keyring"]
    fn a_full_size_token_survives_a_keyring_round_trip() {
        // A Microsoft refresh token is roughly this long; three of these in one
        // JSON blob is what exceeded the 2560-character limit.
        let secrets = StoredSecrets {
            msa_access_token: "a".repeat(1900),
            msa_refresh_token: "r".repeat(2200),
            mc_access_token: "m".repeat(800),
        };
        let uuid = format!("test-{}", std::process::id());

        save(&uuid, &secrets).expect("a token this size must be storable");
        let loaded = load(&uuid).expect("reading back must not fail");
        assert_eq!(loaded, Some(secrets));

        delete(&uuid).expect("cleanup must succeed");
        assert_eq!(load(&uuid).unwrap(), None, "nothing may be left behind");
    }

    /// A shorter value must not leave a longer one's tail in the keyring.
    #[test]
    #[ignore = "needs an unlocked OS keyring"]
    fn overwriting_with_a_shorter_token_clears_the_old_chunks() {
        let uuid = format!("test-chunk-{}", std::process::id());
        let long = StoredSecrets {
            msa_access_token: "x".repeat(5000),
            msa_refresh_token: "y".repeat(5000),
            mc_access_token: "z".repeat(5000),
        };
        let short = StoredSecrets {
            msa_access_token: "short".to_owned(),
            msa_refresh_token: "short".to_owned(),
            mc_access_token: "short".to_owned(),
        };

        save(&uuid, &long).unwrap();
        save(&uuid, &short).unwrap();
        assert_eq!(load(&uuid).unwrap(), Some(short), "the stale tail must be gone");

        delete(&uuid).unwrap();
    }
}
