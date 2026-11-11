//! Authentication: Microsoft OAuth, Yggdrasil, and account storage.

pub mod account;
pub mod authlib;
pub mod microsoft;
pub mod secret;
pub mod xbox;
pub mod yggdrasil;

pub use account::{
    ensure_fresh, is_configured, launch_profile, minecraft_token, remove as remove_account,
    sign_in, sign_in_with_hint, sign_in_yggdrasil,
};
pub use authlib::{agent_path, ensure_agent, javaagent_argument, prefetched_property};
pub use microsoft::{
    MANGO_CLIENT_ID, MsTokens, Pkce, authorize_url, configured_client_id, exchange_code,
    interactive_login, refresh as refresh_ms_token,
};
pub use secret::StoredSecrets;
pub use xbox::{AuthErrorKind, AuthFailure, MinecraftSession, authenticate, dash_uuid};
pub use yggdrasil::YggdrasilSession;

/// Turn an auth error into a message a player can act on.
///
/// The Xbox/Minecraft chain already produces actionable text, so this only adds a
/// hint for the two failures that are the launcher's fault rather than the
/// player's: a missing client ID and a client ID Mojang has not approved yet.
pub fn describe_error(error: &crate::error::MangoError) -> String {
    let text = error.to_string();

    if text.contains("no Microsoft client ID") {
        return format!(
            "{text}. Set MANGO_MSA_CLIENT_ID or create msa.json in the launcher data directory."
        );
    }

    text
}
