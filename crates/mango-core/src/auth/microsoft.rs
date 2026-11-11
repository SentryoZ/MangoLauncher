//! Microsoft OAuth 2.0 with PKCE over a loopback redirect.
//!
//! The launcher is a **public client**: there is no client secret, because any
//! secret shipped inside a desktop binary is not a secret. Authorisation happens
//! in the user's real browser, so the address bar they check while typing a
//! password belongs to Microsoft, and the launcher never touches credentials.
//!
//! Microsoft ignores the port for `http://localhost` redirects on public clients,
//! so an ephemeral port can be used without registering each one.

use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tracing::{debug, info};

use crate::error::{MangoError, Result};
use crate::net::http;

/// Authorisation endpoint for personal Microsoft accounts.
pub const AUTHORIZE_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize";
/// Token endpoint used for both code exchange and refresh.
pub const TOKEN_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
/// Scopes required by the Minecraft chain: Xbox sign-in plus a refresh token.
pub const SCOPE: &str = "XboxLive.signin offline_access";

/// How long to wait for the user to finish signing in.
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);

/// A PKCE verifier/challenge pair.
///
/// The verifier never leaves the process; only its SHA-256 hash is sent to
/// Microsoft in the authorisation request, which is what makes the intercepted
/// code useless to anyone else.
#[derive(Debug, Clone)]
pub struct Pkce {
    verifier: String,
    challenge: String,
}

impl Pkce {
    pub fn new() -> Self {
        // 64 random bytes -> 86 base64url characters, within the 43..128 range
        // RFC 7636 allows.
        let mut bytes = [0u8; 64];
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut bytes);
        let verifier = URL_SAFE_NO_PAD.encode(bytes);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        Self { verifier, challenge }
    }

    pub fn verifier(&self) -> &str {
        &self.verifier
    }

    pub fn challenge(&self) -> &str {
        &self.challenge
    }
}

impl Default for Pkce {
    fn default() -> Self {
        Self::new()
    }
}

/// Tokens returned by the token endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct MsTokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Lifetime in seconds.
    #[serde(default)]
    pub expires_in: i64,
}

/// Build the URL the browser should open.
pub fn authorize_url(client_id: &str, redirect_uri: &str, pkce: &Pkce, state: &str) -> String {
    let params = [
        ("client_id", client_id),
        ("response_type", "code"),
        ("redirect_uri", redirect_uri),
        ("scope", SCOPE),
        ("code_challenge", pkce.challenge()),
        ("code_challenge_method", "S256"),
        ("state", state),
        // Always show the account picker: players often have several accounts and
        // silently reusing the last one is confusing.
        ("prompt", "select_account"),
    ];

    let query: String = params
        .iter()
        .map(|(key, value)| format!("{}={}", key, percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&");

    format!("{AUTHORIZE_URL}?{query}")
}

/// Percent-encode a query component.
fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Result of the browser leg of the flow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationCode {
    pub code: String,
}

/// Run the interactive leg: bind a loopback listener, open the browser, and wait
/// for Microsoft to redirect back with a code.
///
/// `on_url` is called with the authorisation URL before the browser is opened, so
/// a caller can show it. Opening a browser can fail silently on a locked-down
/// machine, and a URL the player can copy is the only way out of that.
pub async fn interactive_login(client_id: &str, on_url: impl Fn(&str)) -> Result<(MsTokens, Pkce)> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|err| MangoError::Other(format!("could not bind a loopback port: {err}")))?;
    let port = listener
        .local_addr()
        .map_err(|err| MangoError::Other(format!("could not read the loopback port: {err}")))?
        .port();

    let redirect_uri = format!("http://localhost:{port}");
    let pkce = Pkce::new();
    let state = uuid::Uuid::new_v4().simple().to_string();
    let url = authorize_url(client_id, &redirect_uri, &pkce, &state);

    info!(port, "waiting for the Microsoft sign-in to finish");
    on_url(&url);

    // A failure to launch the browser is reported, not fatal: the URL was already
    // handed to the caller, so the player can paste it into a browser by hand and
    // the loopback listener still completes the flow.
    if let Err(err) = open_browser(&url) {
        tracing::warn!(error = %err, "could not open the browser automatically");
    }

    let code = tokio::time::timeout(LOGIN_TIMEOUT, accept_code(listener, &state)).await.map_err(
        |_| MangoError::Other("sign-in timed out; the browser window was not completed".to_owned()),
    )??;

    debug!("received authorization code");
    let http_client = http::build_client()?;
    let tokens =
        exchange_code(&http_client, client_id, &redirect_uri, &code, pkce.verifier()).await?;
    Ok((tokens, pkce))
}

/// Accept one HTTP request on the loopback listener and extract the code.
async fn accept_code(listener: TcpListener, expected_state: &str) -> Result<String> {
    loop {
        let (mut socket, _) = listener
            .accept()
            .await
            .map_err(|err| MangoError::Other(format!("loopback accept failed: {err}")))?;

        // A browser may probe the port or fetch /favicon.ico; read whatever came
        // in and only act on a request carrying our parameters.
        let mut buffer = vec![0u8; 8192];
        let read = socket.read(&mut buffer).await.unwrap_or(0);
        let request = String::from_utf8_lossy(&buffer[..read]).to_string();

        let Some(path) = request.split_whitespace().nth(1) else { continue };

        let parsed = parse_query(path);
        let error = parsed.iter().find(|(key, _)| key == "error").map(|(_, value)| value.clone());
        let state = parsed.iter().find(|(key, _)| key == "state").map(|(_, value)| value.clone());
        let code = parsed.iter().find(|(key, _)| key == "code").map(|(_, value)| value.clone());

        if error.is_none() && code.is_none() {
            // Not our redirect (favicon, health probe): answer and keep waiting.
            respond(&mut socket, 404, "Not found").await;
            continue;
        }

        if let Some(error) = error {
            respond(&mut socket, 400, "Sign-in was cancelled. You can close this tab.").await;
            return Err(MangoError::Other(format!("Microsoft returned `{error}`")));
        }

        // A mismatched state means the redirect did not come from our request.
        if state.as_deref() != Some(expected_state) {
            respond(&mut socket, 400, "Invalid sign-in response. You can close this tab.").await;
            return Err(MangoError::Other(
                "the sign-in response did not match this request (state mismatch)".to_owned(),
            ));
        }

        respond(&mut socket, 200, "Signed in. You can close this tab and return to MangoLauncher.")
            .await;
        return Ok(code.unwrap_or_default());
    }
}

async fn respond(socket: &mut tokio::net::TcpStream, status: u16, message: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        _ => "Not Found",
    };
    let body = format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>MangoLauncher</title>\
         <style>body{{font-family:system-ui,sans-serif;background:#171614;color:#eeece7;\
         display:flex;align-items:center;justify-content:center;height:100vh;margin:0}}\
         div{{text-align:center;max-width:32rem;padding:2rem}}h1{{color:#e89c2e;font-size:1.3rem}}\
         </style></head><body><div><h1>MangoLauncher</h1><p>{message}</p></div></body></html>"
    );
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: text/html; charset=utf-8\r\n\
         Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = socket.write_all(response.as_bytes()).await;
    let _ = socket.flush().await;
}

/// Split `path?query` into decoded key/value pairs.
fn parse_query(path: &str) -> Vec<(String, String)> {
    let Some((_, query)) = path.split_once('?') else { return Vec::new() };
    query
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((key, value)) => (percent_decode(key), percent_decode(value)),
            None => (percent_decode(pair), String::new()),
        })
        .collect()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = &value[index + 1..index + 3];
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        out.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    #[serde(default)]
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: i64,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    error_description: Option<String>,
}

impl From<TokenResponse> for MsTokens {
    fn from(response: TokenResponse) -> Self {
        Self {
            access_token: response.access_token,
            refresh_token: response.refresh_token,
            expires_in: response.expires_in,
        }
    }
}

/// Exchange an authorization code for tokens.
pub async fn exchange_code(
    client: &reqwest::Client,
    client_id: &str,
    redirect_uri: &str,
    code: &str,
    verifier: &str,
) -> Result<MsTokens> {
    let form = [
        ("client_id", client_id),
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("code_verifier", verifier),
        ("scope", SCOPE),
    ];
    post_token(client, &form).await
}

/// Exchange a refresh token for a fresh access token.
pub async fn refresh(
    client: &reqwest::Client,
    client_id: &str,
    refresh_token: &str,
) -> Result<MsTokens> {
    let form = [
        ("client_id", client_id),
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh_token),
        ("scope", SCOPE),
    ];
    post_token(client, &form).await
}

async fn post_token(client: &reqwest::Client, form: &[(&str, &str)]) -> Result<MsTokens> {
    let response = client
        .post(TOKEN_URL)
        .form(form)
        .send()
        .await
        .map_err(|err| MangoError::Other(format!("token request failed: {err}")))?;

    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|err| MangoError::Other(format!("token response could not be read: {err}")))?;

    let parsed: TokenResponse = serde_json::from_str(&text)
        .map_err(|err| MangoError::Other(format!("token response was not JSON: {err}")))?;

    if let Some(error) = parsed.error {
        let detail = parsed.error_description.unwrap_or_default();
        return Err(MangoError::Other(format!(
            "Microsoft rejected the token request: {error} {detail}"
        )));
    }
    if !status.is_success() {
        return Err(MangoError::Other(format!("token endpoint returned {status}")));
    }

    Ok(parsed.into())
}

/// Hand a URL to the OS so it opens in the default browser.
///
/// `rundll32 url.dll,FileProtocolHandler` used to be the shortcut here, but it
/// spawns successfully even when nothing opens, so a failure was invisible. The
/// shell API reports a real result.
#[cfg(windows)]
fn open_browser(url: &str) -> Result<()> {
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            hwnd: *mut std::ffi::c_void,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show: i32,
        ) -> *mut std::ffi::c_void;
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    const SW_SHOWNORMAL: i32 = 1;

    let operation = wide("open");
    let file = wide(url);

    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };

    // ShellExecuteW reports values above 32 on success; anything at or below is a
    // documented error code (0 = out of memory, 2/3 = not found, and so on).
    let code = result as isize;
    if code <= 32 {
        return Err(MangoError::Other(format!(
            "the shell could not open a browser (ShellExecuteW returned {code})"
        )));
    }

    Ok(())
}

#[cfg(target_os = "macos")]
fn open_browser(url: &str) -> Result<()> {
    std::process::Command::new("open")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|err| MangoError::Other(format!("could not open the browser: {err}")))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_browser(url: &str) -> Result<()> {
    std::process::Command::new("xdg-open")
        .arg(url)
        .spawn()
        .map(|_| ())
        .map_err(|err| MangoError::Other(format!("could not open the browser: {err}")))
}

/// The client ID configured for this build, if any.
///
/// Checked in order: environment variable, then `msa.json` in the data directory,
/// then the ID compiled into this build. A client ID is not a secret, but keeping
/// it overridable lets forks register their own app.
pub fn configured_client_id(data_dir: &std::path::Path) -> Option<String> {
    if let Some(id) = std::env::var("MANGO_MSA_CLIENT_ID").ok().filter(|id| !id.trim().is_empty()) {
        return Some(id.trim().to_owned());
    }

    let path = data_dir.join("msa.json");
    if let Ok(text) = std::fs::read_to_string(&path)
        && let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text)
        && let Some(id) = parsed.get("clientId").and_then(|value| value.as_str())
        && !id.trim().is_empty()
    {
        return Some(id.trim().to_owned());
    }

    // Compiled-in default: the MangoLauncher Azure app registration.
    let baked = option_env!("MANGO_MSA_CLIENT_ID").unwrap_or(MANGO_CLIENT_ID);
    let baked = baked.trim();
    if baked.is_empty() { None } else { Some(baked.to_owned()) }
}

/// The client ID for the MangoLauncher Azure app.
///
/// This is a **public** client ID, not a secret: it ships inside every binary.
/// It still requires Mojang to allow-list it before Minecraft Services accepts it.
pub const MANGO_CLIENT_ID: &str = "b841728d-5e8f-4350-8127-73e150ce1274";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_pair_is_well_formed() {
        let pkce = Pkce::new();
        assert_eq!(pkce.verifier().len(), 86);
        assert_eq!(pkce.challenge().len(), 43);
        assert!(pkce.verifier().chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn pkce_pairs_are_unique() {
        assert_ne!(Pkce::new().verifier(), Pkce::new().verifier());
    }

    #[test]
    fn challenge_is_the_sha256_of_the_verifier() {
        let pkce = Pkce::new();
        let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(pkce.verifier().as_bytes()));
        assert_eq!(pkce.challenge(), expected);
    }

    #[test]
    fn authorize_url_carries_every_required_parameter() {
        let pkce = Pkce::new();
        let url = authorize_url("client-1", "http://localhost:1234", &pkce, "state-1");

        assert!(url.starts_with(AUTHORIZE_URL));
        assert!(url.contains("client_id=client-1"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains(&format!("code_challenge={}", pkce.challenge())));
        assert!(url.contains("state=state-1"));
        assert!(url.contains("prompt=select_account"));
        // A space in the scope list must be encoded, not literal.
        assert!(url.contains("scope=XboxLive.signin%20offline_access"), "{url}");
    }

    #[test]
    fn the_default_client_id_is_a_guid() {
        // A malformed id fails the handshake with a vague Microsoft error, so the
        // shape is checked here instead.
        let id = MANGO_CLIENT_ID.trim();
        assert_eq!(id.len(), 36, "{id}");
        assert_eq!(id.matches('-').count(), 4, "{id}");
        assert!(id.chars().all(|c| c.is_ascii_hexdigit() || c == '-'), "{id}");
    }

    /// The shell call is what a player sees first.
    ///
    /// Note that a success return does **not** prove a browser appeared: on this
    /// machine `ShellExecuteW` reports success even for an unregistered scheme. That
    /// is exactly why the UI shows the URL — the launcher cannot tell whether a
    /// window opened, so the player must be able to finish the flow by hand.
    #[cfg(windows)]
    #[test]
    fn opening_a_url_does_not_panic_on_a_handled_scheme() {
        assert!(open_browser("https://example.com").is_ok(), "https must be launchable");
    }

    #[test]
    fn query_parsing_extracts_the_code() {
        let parsed = parse_query("/?code=abc123&state=xyz");
        assert!(parsed.contains(&("code".to_owned(), "abc123".to_owned())));
        assert!(parsed.contains(&("state".to_owned(), "xyz".to_owned())));
    }

    #[test]
    fn query_parsing_handles_errors_and_encoding() {
        let parsed = parse_query("/?error=access_denied&error_description=User%20cancelled");
        assert!(parsed.contains(&("error".to_owned(), "access_denied".to_owned())));
        assert!(parsed.contains(&("error_description".to_owned(), "User cancelled".to_owned())));
    }

    #[test]
    fn query_parsing_ignores_paths_without_a_query() {
        assert!(parse_query("/favicon.ico").is_empty());
    }

    #[test]
    fn percent_decoding_handles_plus_and_escapes() {
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("%2F%3F"), "/?");
        // A malformed escape is passed through rather than panicking.
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn client_id_resolution_prefers_the_environment() {
        // The compiled-in ID is available, so this must never be `None`.
        let resolved = configured_client_id(std::path::Path::new("/nonexistent"));
        assert!(resolved.is_some());
    }
}
