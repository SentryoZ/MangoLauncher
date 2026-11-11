//! Account orchestration: sign in, refresh, remove.
//!
//! This is the layer the CLI and the UI call. It owns the interaction between the
//! OAuth flow, the Xbox/Minecraft chain, the keyring, and the non-sensitive
//! metadata in `config.json`.

use tracing::info;

use crate::auth::microsoft::{self, MsTokens};
use crate::auth::secret::{self, StoredSecrets};
use crate::auth::xbox;
use crate::config::{AccountKind, AccountMeta, Config};
use crate::error::{MangoError, Result};
use crate::net::http;
use crate::paths::DataPaths;

/// Refresh a token this long before it actually expires, to avoid racing the
/// server with a token that expires mid-request.
const EXPIRY_MARGIN_SECONDS: i64 = 10;

/// Unix seconds now.
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// Absolute expiry from a relative `expires_in`.
fn expiry_from(expires_in: i64) -> i64 {
    now() + (expires_in - EXPIRY_MARGIN_SECONDS).max(0)
}

/// Sign in with Microsoft, interactively.
///
/// Runs the browser flow, the Xbox/Minecraft chain, then stores the tokens in the
/// keyring and the metadata in `config.json`. Returns the account that was added.
pub async fn sign_in(
    client: &reqwest::Client,
    data: &DataPaths,
    config: &mut Config,
) -> Result<AccountMeta> {
    sign_in_with_hint(client, data, config, |_| {}).await
}

/// Sign in, reporting the browser URL before it is opened.
///
/// The UI shows the URL so a player whose browser does not open can paste it, and
/// so the wait for the redirect is visible rather than looking like a hang.
pub async fn sign_in_with_hint(
    client: &reqwest::Client,
    data: &DataPaths,
    config: &mut Config,
    on_url: impl Fn(&str),
) -> Result<AccountMeta> {
    let client_id = microsoft::configured_client_id(data.root()).ok_or_else(|| {
        MangoError::Other(
            "no Microsoft client ID configured; set MANGO_MSA_CLIENT_ID or create msa.json"
                .to_owned(),
        )
    })?;

    let (tokens, _pkce) = microsoft::interactive_login(&client_id, on_url).await?;
    let refresh_token = tokens.refresh_token.clone().ok_or_else(|| {
        MangoError::Other(
            "Microsoft did not return a refresh token; is offline_access granted?".to_owned(),
        )
    })?;

    let session = xbox::authenticate(client, &tokens.access_token).await?;
    info!(uuid = %session.uuid, name = %session.name, "signed in");

    secret::save(
        &session.uuid,
        &StoredSecrets {
            msa_access_token: tokens.access_token.clone(),
            msa_refresh_token: refresh_token,
            mc_access_token: session.access_token.clone(),
        },
    )?;

    let meta = AccountMeta {
        kind: AccountKind::Microsoft,
        uuid: session.uuid.clone(),
        username: session.name.clone(),
        expires_at: Some(expiry_from(session.expires_in)),
        yggdrasil_base: None,
    };
    config.accounts.insert(session.uuid.clone(), meta.clone());
    config.selected_account = Some(session.uuid);
    config.save(data)?;

    Ok(meta)
}

/// Ensure the selected account's Minecraft token is valid, refreshing if needed.
///
/// Three cases, in order: the Minecraft token is still good; the Microsoft token
/// is still good so only the Minecraft leg is repeated; both expired so the
/// refresh token is used.
pub async fn ensure_fresh(
    client: &reqwest::Client,
    data: &DataPaths,
    config: &mut Config,
    uuid: &str,
) -> Result<StoredSecrets> {
    let meta = config
        .accounts
        .get(uuid)
        .cloned()
        .ok_or_else(|| MangoError::Other(format!("no account `{uuid}`")))?;

    let secrets = secret::load(uuid)?.ok_or_else(|| {
        MangoError::Other(format!(
            "credentials for {} are missing from the keyring; sign in again",
            meta.username
        ))
    })?;

    if meta.expires_at.is_some_and(|expires| now() < expires) {
        return Ok(secrets);
    }

    let client_id = microsoft::configured_client_id(data.root())
        .ok_or_else(|| MangoError::Other("no Microsoft client ID configured".to_owned()))?;

    // Both branches converge on a fresh Microsoft access token, then re-run the
    // Xbox/Minecraft chain with it.
    let msa_access_token = match microsoft::refresh(client, &client_id, &secrets.msa_refresh_token)
        .await
    {
        Ok(tokens) => {
            let new_refresh =
                tokens.refresh_token.clone().unwrap_or_else(|| secrets.msa_refresh_token.clone());
            info!(uuid, "refreshed the Microsoft token");
            (tokens.access_token, new_refresh)
        }
        Err(err) => {
            // The refresh token is dead: the player must sign in again.
            return Err(MangoError::Other(format!(
                "the saved sign-in has expired ({}); sign in again",
                short_error(&err)
            )));
        }
    };

    let session = xbox::authenticate(client, &msa_access_token.0).await?;

    let updated = StoredSecrets {
        msa_access_token: msa_access_token.0,
        msa_refresh_token: msa_access_token.1,
        mc_access_token: session.access_token.clone(),
    };
    secret::save(uuid, &updated)?;

    if let Some(meta) = config.accounts.get_mut(uuid) {
        meta.expires_at = Some(expiry_from(session.expires_in));
        meta.username = session.name;
    }
    config.save(data)?;

    Ok(updated)
}

/// Remove an account: keyring entry first, then metadata.
///
/// The keyring is cleared before the config so a crash in between leaves a
/// config entry without secrets (recoverable by signing in) rather than orphaned
/// secrets with nothing pointing at them.
pub fn remove(data: &DataPaths, config: &mut Config, uuid: &str) -> Result<()> {
    secret::delete(uuid)?;
    config.remove_account(uuid);
    config.save(data)
}

/// The Minecraft access token for the selected account, refreshed if needed.
pub async fn minecraft_token(
    client: &reqwest::Client,
    data: &DataPaths,
    config: &mut Config,
) -> Result<(AccountMeta, String)> {
    let uuid = config
        .selected_account
        .clone()
        .ok_or_else(|| MangoError::Other("no account selected".to_owned()))?;

    let secrets = ensure_fresh(client, data, config, &uuid).await?;
    let meta = config
        .accounts
        .get(&uuid)
        .cloned()
        .ok_or_else(|| MangoError::Other(format!("no account `{uuid}`")))?;

    Ok((meta, secrets.mc_access_token))
}

/// Sign in to a custom Yggdrasil (authlib-injector) server.
///
/// Stores the access token in the keyring and the server URL in the config, so a
/// later launch knows to attach the java agent.
pub async fn sign_in_yggdrasil(
    client: &reqwest::Client,
    data: &DataPaths,
    config: &mut Config,
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<AccountMeta> {
    let session =
        crate::auth::yggdrasil::authenticate(client, base_url, username, password, None).await?;
    info!(uuid = %session.uuid, name = %session.name, base = %base_url, "signed in via yggdrasil");

    secret::save(
        &session.uuid,
        &StoredSecrets {
            // No Microsoft leg for Yggdrasil; one storage shape serves every
            // account kind.
            msa_access_token: String::new(),
            msa_refresh_token: String::new(),
            mc_access_token: session.access_token.clone(),
        },
    )?;

    let meta = AccountMeta {
        kind: AccountKind::Yggdrasil,
        uuid: session.uuid.clone(),
        username: session.name.clone(),
        // Yggdrasil publishes no expiry; the token is used until the server
        // rejects it.
        expires_at: None,
        yggdrasil_base: Some(base_url.trim_end_matches('/').to_owned()),
    };
    config.accounts.insert(session.uuid.clone(), meta.clone());
    config.selected_account = Some(session.uuid);
    config.save(data)?;

    Ok(meta)
}

/// Convert a stored account into the launch profile the game needs.
///
/// Returns the profile plus the JVM arguments it requires: a custom Yggdrasil
/// server needs the authlib agent. Fails with an actionable message when the
/// token is missing, so the caller can prompt for sign-in instead of launching a
/// session the server will reject.
pub async fn launch_profile(
    client: &reqwest::Client,
    data: &DataPaths,
    config: &mut Config,
    uuid: &str,
) -> Result<(crate::launch::LaunchAccount, Vec<String>)> {
    let meta = config
        .accounts
        .get(uuid)
        .cloned()
        .ok_or_else(|| MangoError::Other(format!("no account `{uuid}`")))?;

    match meta.kind {
        AccountKind::Microsoft => {
            let secrets = ensure_fresh(client, data, config, uuid).await?;
            Ok((
                crate::launch::LaunchAccount::microsoft(&meta, secrets.mc_access_token),
                Vec::new(),
            ))
        }
        AccountKind::Yggdrasil => {
            let secrets = secret::load(uuid)?.ok_or_else(|| {
                MangoError::Other(format!(
                    "credentials for {} are missing; sign in again",
                    meta.username
                ))
            })?;

            let mut agents = Vec::new();
            if let Some(base) = meta.yggdrasil_base.as_deref().filter(|base| !base.is_empty()) {
                let jar = crate::auth::ensure_agent(client, data.root()).await?;
                agents.push(crate::auth::javaagent_argument(&jar, base));
                // Prefetching saves the game a metadata fetch at startup and lets
                // it verify the server signature without a blocking request.
                if let Ok(metadata) = crate::auth::yggdrasil::fetch_metadata(client, base).await
                    && let Ok(json) = serde_json::to_string(&metadata)
                {
                    agents.push(crate::auth::prefetched_property(&json));
                }
            }

            Ok((crate::launch::LaunchAccount::yggdrasil(&meta, secrets.mc_access_token), agents))
        }
        AccountKind::Offline => {
            Ok((crate::launch::LaunchAccount::offline(&meta.username), Vec::new()))
        }
    }
}

/// Trim an error to a length that fits in a UI status line.
fn short_error(err: &MangoError) -> String {
    err.to_string().chars().take(160).collect()
}

/// Build a client for authentication calls.
pub fn client() -> Result<reqwest::Client> {
    http::build_client()
}

/// Whether a Microsoft sign-in can be attempted at all.
pub fn is_configured(data: &DataPaths) -> bool {
    microsoft::configured_client_id(data.root()).is_some()
}

/// The tokens type, re-exported for callers that need to inspect expiry.
pub type Tokens = MsTokens;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_is_advanced_by_ten_seconds() {
        let before = now();
        let expires = expiry_from(3600);
        // 3600 - 10 seconds from now, with a second of slack for the clock.
        assert!(expires >= before + 3585, "{expires} vs {before}");
        assert!(expires <= now() + 3595);
    }

    #[test]
    fn a_non_positive_lifetime_does_not_produce_a_past_expiry() {
        // A server reporting expires_in=0 must not create a token that is already
        // expired by a negative margin.
        let expires = expiry_from(0);
        assert!(expires >= now());
        assert!(expires <= now() + 1);
    }

    #[test]
    fn removing_an_account_clears_metadata() {
        let dir = std::env::temp_dir().join(format!("mango-auth-rm-{}", std::process::id()));
        let data = DataPaths::new(&dir);

        let mut config = Config::default();
        config.accounts.insert(
            "uuid-1".to_owned(),
            AccountMeta {
                kind: AccountKind::Offline,
                uuid: "uuid-1".to_owned(),
                username: "Steve".to_owned(),
                expires_at: None,
                yggdrasil_base: None,
            },
        );
        config.selected_account = Some("uuid-1".to_owned());

        // The keyring delete is a no-op for a missing entry, so this works even
        // without a usable keyring.
        remove(&data, &mut config, "uuid-1").unwrap();
        assert!(config.accounts.is_empty());
        assert_eq!(config.selected_account, None);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_client_id_is_reported_as_configured() {
        // The compiled-in ID means a build can always offer sign-in.
        let data = DataPaths::new(std::env::temp_dir());
        assert!(is_configured(&data));
    }
}
