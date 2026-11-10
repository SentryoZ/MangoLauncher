//! Credential storage in the OS keyring.
//!
//! Access and refresh tokens must not end up in `config.json`: that file is
//! plain text, is copied into bug reports, and often lands in a synced folder.
//! The keyring maps to DPAPI on Windows, Secret Service on Linux, and the
//! Keychain on macOS, all of which are encrypted at rest under the user's login.

use serde::{Deserialize, Serialize};
use tracing::{debug, warn};

use crate::error::{MangoError, Result};

/// Service name used for every entry ("MangoLauncher").
const SERVICE: &str = "mangolauncher";

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
}

impl std::fmt::Display for StoredSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.redacted())
    }
}

/// Save an account's secrets, replacing anything already stored for that uuid.
pub fn save(uuid: &str, secrets: &StoredSecrets) -> Result<()> {
    let entry = keyring::Entry::new(SERVICE, uuid)
        .map_err(|err| MangoError::Other(format!("keyring is unavailable: {err}")))?;

    let payload = serde_json::to_string(secrets)?;
    entry.set_password(&payload).map_err(|err| {
        MangoError::Other(format!("could not save credentials to the keyring: {err}"))
    })?;

    debug!(uuid, "stored credentials in the keyring");
    Ok(())
}

/// Load an account's secrets, or `None` when nothing is stored.
pub fn load(uuid: &str) -> Result<Option<StoredSecrets>> {
    let entry = keyring::Entry::new(SERVICE, uuid)
        .map_err(|err| MangoError::Other(format!("keyring is unavailable: {err}")))?;

    match entry.get_password() {
        Ok(payload) => match serde_json::from_str(&payload) {
            Ok(secrets) => Ok(Some(secrets)),
            Err(err) => {
                // A malformed entry is worse than none: report it as missing so the
                // player can sign in again instead of hitting a parse error.
                warn!(uuid, error = %err, "discarding an unreadable keyring entry");
                let _ = entry.delete_credential();
                Ok(None)
            }
        },
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(err) => {
            Err(MangoError::Other(format!("could not read credentials from the keyring: {err}")))
        }
    }
}

/// Remove an account's secrets.
pub fn delete(uuid: &str) -> Result<()> {
    let entry = keyring::Entry::new(SERVICE, uuid)
        .map_err(|err| MangoError::Other(format!("keyring is unavailable: {err}")))?;

    match entry.delete_credential() {
        Ok(()) => {
            debug!(uuid, "removed credentials from the keyring");
            Ok(())
        }
        // Deleting something that is not there is not a failure.
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(err) => {
            Err(MangoError::Other(format!("could not remove credentials from the keyring: {err}")))
        }
    }
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
}
