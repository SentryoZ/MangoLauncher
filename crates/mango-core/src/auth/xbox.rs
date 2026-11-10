//! Xbox Live → XSTS → Minecraft Services token chain.
//!
//! Microsoft's token cannot be used by Minecraft directly. It has to be traded for
//! an Xbox Live token, then an XSTS token, and only then for a Minecraft access
//! token. Each stage reports its own errors, and the XSTS stage has specific
//! failures players actually hit (no Xbox profile, child account, ban).

use serde::Deserialize;
use tracing::debug;

use crate::error::{MangoError, Result};

const XBL_AUTH_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_AUTH_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const MC_LOGIN_URL: &str = "https://api.minecraftservices.com/authentication/login_with_xbox";
const MC_PROFILE_URL: &str = "https://api.minecraftservices.com/minecraft/profile";
const RELYING_PARTY: &str = "rp://api.minecraftservices.com/";

/// A Minecraft session, ready to be handed to the game.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MinecraftSession {
    pub access_token: String,
    /// Lifetime in seconds, as reported by Minecraft Services.
    pub expires_in: i64,
    pub uuid: String,
    pub name: String,
}

/// Failures a player can act on, translated from XSTS/HTTP responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthErrorKind {
    /// The account has no Xbox profile yet.
    NoXboxAccount,
    /// The account is banned from Xbox Live.
    XboxBanned,
    /// A child account without parental consent.
    Underage,
    /// The account does not own Minecraft: Java Edition.
    NoGameOwnership,
    /// The Azure app has not been allow-listed by Mojang.
    AppNotApproved,
    Unknown,
}

impl AuthErrorKind {
    /// Message shown to the player.
    pub fn message(self) -> &'static str {
        match self {
            Self::NoXboxAccount => {
                "This account has no Xbox profile yet. Sign in at xbox.com once, then try again."
            }
            Self::XboxBanned => "This account is banned from Xbox Live.",
            Self::Underage => {
                "This is a child account. A parent or guardian must grant permission first."
            }
            Self::NoGameOwnership => "This account does not own Minecraft: Java Edition.",
            Self::AppNotApproved => {
                "Microsoft accepted the sign-in, but Mojang has not approved this launcher yet \
                 (HTTP 403). The app is pending review."
            }
            Self::Unknown => "Sign-in failed. Please try again.",
        }
    }

    /// Map an XSTS `XErr` code.
    pub fn from_xsts_code(code: &str) -> Self {
        match code {
            "2148916233" => Self::NoXboxAccount,
            "2148916235" => Self::XboxBanned,
            "2148916238" => Self::Underage,
            _ => Self::Unknown,
        }
    }
}

/// A structured auth failure, so the UI can show something actionable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthFailure {
    pub kind: AuthErrorKind,
    /// Raw detail, for the log.
    pub detail: String,
}

impl std::fmt::Display for AuthFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.kind.message(), self.detail)
    }
}

impl std::error::Error for AuthFailure {}

impl From<AuthFailure> for MangoError {
    fn from(failure: AuthFailure) -> Self {
        MangoError::Other(failure.to_string())
    }
}

#[derive(Debug, Deserialize)]
struct XblResponse {
    #[serde(rename = "Token")]
    token: String,
    #[serde(rename = "DisplayClaims")]
    display_claims: DisplayClaims,
}

#[derive(Debug, Deserialize)]
struct DisplayClaims {
    xui: Vec<Xui>,
}

#[derive(Debug, Deserialize)]
struct Xui {
    uhs: String,
}

#[derive(Debug, Deserialize)]
struct XstsErrorResponse {
    #[serde(rename = "XErr", default)]
    xerr: Option<u64>,
    #[serde(default)]
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct McLoginResponse {
    access_token: String,
    #[serde(default)]
    expires_in: i64,
}

#[derive(Debug, Deserialize)]
struct McProfileResponse {
    id: String,
    name: String,
}

/// Run the whole chain from a Microsoft access token to a Minecraft session.
pub async fn authenticate(
    client: &reqwest::Client,
    msa_access_token: &str,
) -> Result<MinecraftSession> {
    let (xbl_token, uhs) = xbox_live(client, msa_access_token).await?;
    let xsts_token = xsts(client, &xbl_token).await?;
    let (mc_token, expires_in) = minecraft_login(client, &uhs, &xsts_token).await?;
    let (uuid, name) = minecraft_profile(client, &mc_token).await?;

    Ok(MinecraftSession { access_token: mc_token, expires_in, uuid, name })
}

async fn xbox_live(client: &reqwest::Client, msa_access_token: &str) -> Result<(String, String)> {
    let body = serde_json::json!({
        "Properties": {
            "AuthMethod": "RPS",
            // The `d=` prefix marks a Microsoft account ticket rather than an
            // Xbox one.
            "SiteName": "user.auth.xboxlive.com",
            "RpsTicket": format!("d={msa_access_token}")
        },
        "RelyingParty": "http://auth.xboxlive.com",
        "TokenType": "JWT"
    });

    let response = client
        .post(XBL_AUTH_URL)
        .json(&body)
        .send()
        .await
        .map_err(|err| MangoError::Other(format!("Xbox Live request failed: {err}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        return Err(MangoError::Other(format!(
            "Xbox Live rejected the Microsoft token ({status}): {text}"
        )));
    }

    let parsed: XblResponse = response
        .json()
        .await
        .map_err(|err| MangoError::Other(format!("Xbox Live response was not usable: {err}")))?;

    let uhs = parsed
        .display_claims
        .xui
        .first()
        .map(|xui| xui.uhs.clone())
        .ok_or_else(|| MangoError::Other("Xbox Live returned no user hash".to_owned()))?;

    debug!("xbox live token acquired");
    Ok((parsed.token, uhs))
}

async fn xsts(client: &reqwest::Client, xbl_token: &str) -> Result<String> {
    let body = serde_json::json!({
        "Properties": { "SandboxId": "RETAIL", "UserTokens": [xbl_token] },
        "RelyingParty": RELYING_PARTY,
        "TokenType": "JWT"
    });

    let response = client
        .post(XSTS_AUTH_URL)
        .json(&body)
        .send()
        .await
        .map_err(|err| MangoError::Other(format!("XSTS request failed: {err}")))?;

    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|err| MangoError::Other(format!("XSTS response could not be read: {err}")))?;

    if !status.is_success() {
        // XSTS reports actionable problems as a numeric XErr in the body.
        let parsed: XstsErrorResponse =
            serde_json::from_str(&text).unwrap_or(XstsErrorResponse { xerr: None, message: None });

        let failure = match parsed.xerr {
            Some(code) => AuthErrorKind::from_xsts_code(&code.to_string()),
            None => AuthErrorKind::Unknown,
        };
        return Err(AuthFailure {
            kind: failure,
            detail: format!(
                "XSTS {status}: {}",
                parsed.message.unwrap_or_else(|| text.chars().take(200).collect())
            ),
        }
        .into());
    }

    let parsed: XblResponse = serde_json::from_str(&text)
        .map_err(|err| MangoError::Other(format!("XSTS response was not usable: {err}")))?;
    Ok(parsed.token)
}

async fn minecraft_login(
    client: &reqwest::Client,
    uhs: &str,
    xsts_token: &str,
) -> Result<(String, i64)> {
    let identity = format!("XBL3.0 x={uhs};{xsts_token}");

    let response = client
        .post(MC_LOGIN_URL)
        .json(&serde_json::json!({ "identityToken": identity }))
        .send()
        .await
        .map_err(|err| MangoError::Other(format!("Minecraft login request failed: {err}")))?;

    let status = response.status();
    if status == reqwest::StatusCode::FORBIDDEN {
        // The usual cause: this Azure app has not been allow-listed by Mojang.
        let text = response.text().await.unwrap_or_default();
        return Err(AuthFailure {
            kind: AuthErrorKind::AppNotApproved,
            detail: format!("HTTP 403 from Minecraft Services: {text}"),
        }
        .into());
    }
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(MangoError::Other(format!(
            "Minecraft Services rejected the XSTS token ({status}): {text}"
        )));
    }

    let parsed: McLoginResponse = response.json().await.map_err(|err| {
        MangoError::Other(format!("Minecraft login response was not usable: {err}"))
    })?;

    Ok((parsed.access_token, parsed.expires_in))
}

async fn minecraft_profile(
    client: &reqwest::Client,
    access_token: &str,
) -> Result<(String, String)> {
    let response = client
        .get(MC_PROFILE_URL)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|err| MangoError::Other(format!("Minecraft profile request failed: {err}")))?;

    let status = response.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        // A 404 here means the account has no Java Edition profile.
        return Err(AuthFailure {
            kind: AuthErrorKind::NoGameOwnership,
            detail: "HTTP 404 from /minecraft/profile".to_owned(),
        }
        .into());
    }
    if !status.is_success() {
        let text = response.text().await.unwrap_or_default();
        return Err(MangoError::Other(format!(
            "could not read the Minecraft profile ({status}): {text}"
        )));
    }

    let profile: McProfileResponse = response
        .json()
        .await
        .map_err(|err| MangoError::Other(format!("profile response was not usable: {err}")))?;

    // Minecraft returns the UUID without dashes; the game and Mojang APIs both
    // accept the dashed form and it is what config files elsewhere use.
    Ok((dash_uuid(&profile.id), profile.name))
}

/// Insert dashes into a 32-character UUID.
pub fn dash_uuid(raw: &str) -> String {
    let hex: String = raw.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 32 {
        return raw.to_owned();
    }
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xsts_codes_map_to_actionable_errors() {
        assert_eq!(AuthErrorKind::from_xsts_code("2148916233"), AuthErrorKind::NoXboxAccount);
        assert_eq!(AuthErrorKind::from_xsts_code("2148916235"), AuthErrorKind::XboxBanned);
        assert_eq!(AuthErrorKind::from_xsts_code("2148916238"), AuthErrorKind::Underage);
        assert_eq!(AuthErrorKind::from_xsts_code("999"), AuthErrorKind::Unknown);
    }

    #[test]
    fn every_error_kind_has_a_message() {
        for kind in [
            AuthErrorKind::NoXboxAccount,
            AuthErrorKind::XboxBanned,
            AuthErrorKind::Underage,
            AuthErrorKind::NoGameOwnership,
            AuthErrorKind::AppNotApproved,
            AuthErrorKind::Unknown,
        ] {
            assert!(!kind.message().is_empty());
        }
    }

    #[test]
    fn app_not_approved_message_mentions_the_pending_review() {
        assert!(AuthErrorKind::AppNotApproved.message().contains("403"));
        assert!(AuthErrorKind::AppNotApproved.message().contains("review"));
    }

    #[test]
    fn uuids_are_dashed() {
        assert_eq!(
            dash_uuid("069a79f444e94726a5befca90e38aaf5"),
            "069a79f4-44e9-4726-a5be-fca90e38aaf5"
        );
    }

    #[test]
    fn an_already_dashed_uuid_is_normalised() {
        assert_eq!(
            dash_uuid("069a79f4-44e9-4726-a5be-fca90e38aaf5"),
            "069a79f4-44e9-4726-a5be-fca90e38aaf5"
        );
    }

    #[test]
    fn an_unexpected_uuid_is_returned_untouched() {
        assert_eq!(dash_uuid("not-a-uuid"), "not-a-uuid");
    }

    #[test]
    fn xsts_error_responses_parse() {
        let parsed: XstsErrorResponse = serde_json::from_str(
            r#"{"Identity":"0","XErr":2148916233,"Message":"","Redirect":""}"#,
        )
        .unwrap();
        assert_eq!(parsed.xerr, Some(2148916233));
    }

    #[test]
    fn xbl_responses_parse() {
        let parsed: XblResponse =
            serde_json::from_str(r#"{"Token":"jwt","DisplayClaims":{"xui":[{"uhs":"12345"}]}}"#)
                .unwrap();
        assert_eq!(parsed.token, "jwt");
        assert_eq!(parsed.display_claims.xui[0].uhs, "12345");
    }

    #[test]
    fn auth_failure_displays_message_and_detail() {
        let failure =
            AuthFailure { kind: AuthErrorKind::NoXboxAccount, detail: "XSTS 401".to_owned() };
        let text = failure.to_string();
        assert!(text.contains("Xbox profile"));
        assert!(text.contains("XSTS 401"));
    }
}
