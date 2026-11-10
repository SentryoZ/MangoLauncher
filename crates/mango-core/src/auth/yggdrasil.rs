//! Yggdrasil authentication, including custom authlib-injector servers.
//!
//! Yggdrasil is Minecraft's own auth protocol. Mojang's account system no longer
//! accepts password logins, so the only live use is a **custom** auth server run
//! by a community or server owner. The protocol is the same either way; only the
//! base URL differs, which is why one implementation covers both.
//!
//! For a custom server the game also needs `-javaagent:authlib-injector.jar=<url>`
//! so its own authlib redirects to that server; see [`super::authlib`].

use serde::{Deserialize, Serialize};
use tracing::info;

use crate::error::{MangoError, Result};

/// Official Yggdrasil endpoint. Kept for completeness; password login is closed.
pub const MOJANG_YGGDRASIL: &str = "https://authserver.mojang.com";

/// A Yggdrasil session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YggdrasilSession {
    pub access_token: String,
    pub client_token: String,
    pub uuid: String,
    pub name: String,
}

#[derive(Debug, Serialize)]
struct Agent {
    name: &'static str,
    version: u32,
}

#[derive(Debug, Serialize)]
struct AuthenticateRequest<'a> {
    agent: Agent,
    username: &'a str,
    password: &'a str,
    #[serde(rename = "clientToken", skip_serializing_if = "Option::is_none")]
    client_token: Option<&'a str>,
    #[serde(rename = "requestUser")]
    request_user: bool,
}

#[derive(Debug, Serialize)]
struct RefreshRequest<'a> {
    #[serde(rename = "accessToken")]
    access_token: &'a str,
    #[serde(rename = "clientToken")]
    client_token: &'a str,
    #[serde(rename = "requestUser")]
    request_user: bool,
}

#[derive(Debug, Serialize)]
struct ValidateRequest<'a> {
    #[serde(rename = "accessToken")]
    access_token: &'a str,
    #[serde(rename = "clientToken")]
    client_token: &'a str,
}

#[derive(Debug, Deserialize)]
struct AuthResponse {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "clientToken", default)]
    client_token: String,
    #[serde(rename = "selectedProfile", default)]
    selected_profile: Option<Profile>,
    #[serde(rename = "availableProfiles", default)]
    available_profiles: Vec<Profile>,
}

#[derive(Debug, Deserialize)]
struct Profile {
    id: String,
    name: String,
}

impl AuthResponse {
    fn into_session(self) -> Result<YggdrasilSession> {
        let profile = self
            .selected_profile
            .or_else(|| self.available_profiles.into_iter().next())
            .ok_or_else(|| {
                MangoError::Other(
                    "this account has no Minecraft profile; the game is not owned on it".to_owned(),
                )
            })?;

        Ok(YggdrasilSession {
            access_token: self.access_token,
            client_token: self.client_token,
            uuid: super::xbox::dash_uuid(&profile.id),
            name: profile.name,
        })
    }
}

#[derive(Debug, Deserialize)]
struct ErrorResponse {
    #[serde(default)]
    error: Option<String>,
    #[serde(rename = "errorMessage", default)]
    error_message: Option<String>,
}

/// Trim a trailing slash so `{base}/authserver/authenticate` is well formed.
fn base_url(base: &str) -> &str {
    base.trim_end_matches('/')
}

/// Read an error body into a readable message.
fn read_error(status: reqwest::StatusCode, text: &str) -> MangoError {
    match serde_json::from_str::<ErrorResponse>(text) {
        Ok(parsed) => {
            let code = parsed.error.unwrap_or_else(|| status.to_string());
            let message = parsed.error_message.unwrap_or_default();
            let hint = match code.as_str() {
                "ForbiddenOperationException" => " (wrong password, or the account is migrated)",
                "Unauthorized" => " (the token is no longer valid)",
                _ => "",
            };
            MangoError::Other(format!("auth server returned `{code}`: {message}{hint}"))
        }
        Err(_) => MangoError::Other(format!(
            "auth server returned {status}: {}",
            text.chars().take(200).collect::<String>()
        )),
    }
}

/// Sign in with username and password against a Yggdrasil server.
pub async fn authenticate(
    client: &reqwest::Client,
    base: &str,
    username: &str,
    password: &str,
    client_token: Option<&str>,
) -> Result<YggdrasilSession> {
    let url = format!("{}/authserver/authenticate", base_url(base));
    let body = AuthenticateRequest {
        agent: Agent { name: "Minecraft", version: 1 },
        username,
        password,
        client_token,
        request_user: true,
    };

    let response = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|err| MangoError::Other(format!("auth server unreachable: {err}")))?;

    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|err| MangoError::Other(format!("auth response unreadable: {err}")))?;

    if !status.is_success() {
        return Err(read_error(status, &text));
    }

    let parsed: AuthResponse = serde_json::from_str(&text)
        .map_err(|err| MangoError::Other(format!("auth response was not usable: {err}")))?;

    let session = parsed.into_session()?;
    info!(name = %session.name, "yggdrasil sign-in succeeded");
    Ok(session)
}

/// Renew an expired access token without asking for the password again.
pub async fn refresh(
    client: &reqwest::Client,
    base: &str,
    access_token: &str,
    client_token: &str,
) -> Result<YggdrasilSession> {
    let url = format!("{}/authserver/refresh", base_url(base));
    let body = RefreshRequest { access_token, client_token, request_user: true };

    let response = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|err| MangoError::Other(format!("auth server unreachable: {err}")))?;

    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|err| MangoError::Other(format!("auth response unreadable: {err}")))?;

    if !status.is_success() {
        return Err(read_error(status, &text));
    }

    let parsed: AuthResponse = serde_json::from_str(&text)
        .map_err(|err| MangoError::Other(format!("auth response was not usable: {err}")))?;
    parsed.into_session()
}

/// Ask the server whether a token is still valid.
///
/// A `403` is the protocol's way of saying "not valid", not a transport failure,
/// so it is mapped to `Ok(false)` rather than an error.
pub async fn validate(
    client: &reqwest::Client,
    base: &str,
    access_token: &str,
    client_token: &str,
) -> Result<bool> {
    let url = format!("{}/authserver/validate", base_url(base));
    let body = ValidateRequest { access_token, client_token };

    let response = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|err| MangoError::Other(format!("auth server unreachable: {err}")))?;

    Ok(response.status().is_success())
}

/// Invalidate a token, so it cannot be used again.
pub async fn invalidate(
    client: &reqwest::Client,
    base: &str,
    access_token: &str,
    client_token: &str,
) -> Result<()> {
    let url = format!("{}/authserver/invalidate", base_url(base));
    let body = ValidateRequest { access_token, client_token };

    client
        .post(&url)
        .json(&body)
        .send()
        .await
        .map_err(|err| MangoError::Other(format!("auth server unreachable: {err}")))?;
    Ok(())
}

/// Read a server's metadata document.
///
/// Custom auth servers publish this; authlib-injector needs it to know the skin
/// domains and to verify the server's signature.
pub async fn fetch_metadata(client: &reqwest::Client, base: &str) -> Result<serde_json::Value> {
    let url = base_url(base);
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|err| MangoError::Other(format!("auth server unreachable: {err}")))?;

    if !response.status().is_success() {
        return Err(MangoError::Other(format!(
            "auth server metadata returned {}",
            response.status()
        )));
    }

    response
        .json()
        .await
        .map_err(|err| MangoError::Other(format!("auth server metadata was not JSON: {err}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_slashes_are_trimmed() {
        assert_eq!(
            base_url("https://example.com/api/yggdrasil/"),
            "https://example.com/api/yggdrasil"
        );
        assert_eq!(base_url("https://example.com"), "https://example.com");
    }

    #[test]
    fn authenticate_request_serialises_as_the_protocol_expects() {
        let body = AuthenticateRequest {
            agent: Agent { name: "Minecraft", version: 1 },
            username: "steve@example.com",
            password: "secret",
            client_token: Some("token-1"),
            request_user: true,
        };
        let json = serde_json::to_value(&body).unwrap();

        assert_eq!(json["agent"]["name"], "Minecraft");
        assert_eq!(json["agent"]["version"], 1);
        assert_eq!(json["username"], "steve@example.com");
        assert_eq!(json["clientToken"], "token-1");
        assert_eq!(json["requestUser"], true);
    }

    #[test]
    fn a_missing_client_token_is_omitted() {
        let body = AuthenticateRequest {
            agent: Agent { name: "Minecraft", version: 1 },
            username: "u",
            password: "p",
            client_token: None,
            request_user: true,
        };
        let json = serde_json::to_value(&body).unwrap();
        assert!(json.get("clientToken").is_none(), "the server generates one when absent");
    }

    #[test]
    fn a_successful_response_becomes_a_session() {
        let parsed: AuthResponse = serde_json::from_str(
            r#"{"accessToken":"at","clientToken":"ct",
                "selectedProfile":{"id":"069a79f444e94726a5befca90e38aaf5","name":"Steve"}}"#,
        )
        .unwrap();

        let session = parsed.into_session().unwrap();
        assert_eq!(session.access_token, "at");
        assert_eq!(session.client_token, "ct");
        assert_eq!(session.uuid, "069a79f4-44e9-4726-a5be-fca90e38aaf5");
        assert_eq!(session.name, "Steve");
    }

    #[test]
    fn a_profile_without_a_selected_entry_uses_the_first_available() {
        let parsed: AuthResponse = serde_json::from_str(
            r#"{"accessToken":"at","clientToken":"ct",
                "availableProfiles":[{"id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","name":"Alex"}]}"#,
        )
        .unwrap();
        assert_eq!(parsed.into_session().unwrap().name, "Alex");
    }

    #[test]
    fn no_profile_means_the_game_is_not_owned() {
        let parsed: AuthResponse =
            serde_json::from_str(r#"{"accessToken":"at","clientToken":"ct"}"#).unwrap();
        let error = parsed.into_session().unwrap_err();
        assert!(error.to_string().contains("not owned"), "{error}");
    }

    #[test]
    fn error_bodies_become_actionable_messages() {
        let err = read_error(
            reqwest::StatusCode::FORBIDDEN,
            r#"{"error":"ForbiddenOperationException","errorMessage":"Invalid credentials. Invalid username or password."}"#,
        );
        let text = err.to_string();
        assert!(text.contains("Invalid credentials"));
        assert!(text.contains("wrong password"), "a hint should be added: {text}");
    }

    #[test]
    fn a_non_json_error_body_still_reports_the_status() {
        let err = read_error(reqwest::StatusCode::BAD_GATEWAY, "<html>oops</html>");
        assert!(err.to_string().contains("502"));
    }
}
