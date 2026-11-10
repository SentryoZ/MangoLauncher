//! Shared HTTP client.
//!
//! One client per process keeps connection pooling and TLS setup cheap. The
//! user agent is mandatory for the Modrinth API, which rejects anonymous
//! callers.

use std::time::Duration;

use reqwest::{Response, StatusCode};
use tracing::debug;

pub use reqwest::Client;

use crate::error::{MangoError, Result};

/// Default user agent sent with every request.
pub fn user_agent() -> String {
    format!("MangoLauncher/{} (github.com/example/MangoLauncher)", crate::VERSION)
}

/// Build a client with sane timeouts and retries left to the caller.
pub fn build_client() -> Result<Client> {
    Client::builder()
        .user_agent(user_agent())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .pool_max_idle_per_host(8)
        .build()
        .map_err(|err| MangoError::Http(err.to_string()))
}

/// Fetch a URL as bytes, retrying transient failures.
pub async fn get_bytes(client: &Client, url: &str) -> Result<Vec<u8>> {
    let response = get_with_retry(client, url, None).await?;
    Ok(response.bytes().await.map_err(|err| MangoError::Http(err.to_string()))?.to_vec())
}

/// Fetch a URL as JSON.
pub async fn get_json<T: serde::de::DeserializeOwned>(client: &Client, url: &str) -> Result<T> {
    let bytes = get_bytes(client, url).await?;
    serde_json::from_slice(&bytes).map_err(MangoError::from)
}

/// Fetch a URL as JSON, sending one extra header.
///
/// Used for authenticated catalogue APIs such as CurseForge's `x-api-key`.
pub async fn get_json_with_header<T: serde::de::DeserializeOwned>(
    client: &Client,
    url: &str,
    header: &str,
    value: &str,
) -> Result<T> {
    let response = client
        .get(url)
        .header(header, value)
        .send()
        .await
        .map_err(|err| MangoError::Http(format!("{url}: {err}")))?;

    let status = response.status();
    if !status.is_success() {
        let hint = if status == StatusCode::FORBIDDEN {
            " (the API key was rejected or is missing)"
        } else {
            ""
        };
        return Err(MangoError::Http(format!("{url} returned {status}{hint}")));
    }

    let bytes = response.bytes().await.map_err(|err| MangoError::Http(err.to_string()))?;
    serde_json::from_slice(&bytes).map_err(MangoError::from)
}

/// Fetch a URL as text, used for maven metadata and RSS.
pub async fn get_text(client: &Client, url: &str) -> Result<String> {
    let response = get_with_retry(client, url, None).await?;
    response.text().await.map_err(|err| MangoError::Http(err.to_string()))
}

/// Perform a GET with conditional headers, returning body plus `ETag`.
pub async fn get_text_with_etag(
    client: &Client,
    url: &str,
    etag: Option<&str>,
) -> Result<ConditionalResponse> {
    let response = get_with_retry(client, url, etag).await?;

    if response.status() == StatusCode::NOT_MODIFIED {
        return Ok(ConditionalResponse { body: None, etag: etag.map(str::to_owned) });
    }

    let etag_out = header_string(&response, "etag");
    let body = response.text().await.map_err(|err| MangoError::Http(err.to_string()))?;
    Ok(ConditionalResponse { body: Some(body), etag: etag_out })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConditionalResponse {
    /// `None` when the server answered `304 Not Modified`.
    pub body: Option<String>,
    pub etag: Option<String>,
}

fn header_string(response: &Response, name: &str) -> Option<String> {
    response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned)
}

/// GET with exponential backoff on 429/5xx and connection errors.
pub async fn get_with_retry(client: &Client, url: &str, etag: Option<&str>) -> Result<Response> {
    const ATTEMPTS: u32 = 3;

    let mut attempt = 0;
    loop {
        attempt += 1;

        let mut request = client.get(url);
        if let Some(etag) = etag {
            request = request.header(reqwest::header::IF_NONE_MATCH, etag);
        }

        let outcome = request.send().await;
        match outcome {
            Ok(response) => {
                let status = response.status();
                if status.is_success() || status == StatusCode::NOT_MODIFIED {
                    return Ok(response);
                }

                let retryable = status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
                if !retryable || attempt >= ATTEMPTS {
                    return Err(MangoError::Http(format!("{url} returned {status}")));
                }
                debug!(url, %status, attempt, "retrying request after server error");
            }
            Err(err) => {
                if attempt >= ATTEMPTS {
                    return Err(MangoError::Http(format!("{url}: {err}")));
                }
                debug!(url, error = %err, attempt, "retrying request after transport error");
            }
        }

        tokio::time::sleep(backoff(attempt)).await;
    }
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(250 * 2u64.pow(attempt.saturating_sub(1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_agent_mentions_launcher_and_version() {
        let ua = user_agent();
        assert!(ua.starts_with("MangoLauncher/"));
        assert!(ua.contains(crate::VERSION));
    }

    #[test]
    fn backoff_grows_exponentially() {
        assert_eq!(backoff(1), Duration::from_millis(250));
        assert_eq!(backoff(2), Duration::from_millis(500));
        assert_eq!(backoff(3), Duration::from_millis(1000));
    }
}
