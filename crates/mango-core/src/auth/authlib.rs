//! authlib-injector support for custom Yggdrasil servers.
//!
//! Minecraft's client only talks to Mojang's auth server. To use a community auth
//! server, the game is started with authlib-injector as a Java agent, which
//! rewrites its auth calls to the configured base URL. The launcher's job is to
//! fetch the agent jar and pass the right `-javaagent` argument.

use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use tracing::info;

use crate::error::{MangoError, Result};
use crate::net::http;

/// Where the agent jar is published. Pinned to a release rather than "latest" so
/// a new upstream release cannot silently change behaviour.
pub const AUTHLIB_INJECTOR_URL: &str = "https://github.com/yushijinhun/authlib-injector/releases/download/v1.2.5/authlib-injector-1.2.5.jar";

/// File name used inside the launcher's data directory.
pub const AUTHLIB_INJECTOR_FILE: &str = "authlib-injector.jar";

/// Directory holding downloaded Java agents.
pub fn agent_dir(data_root: &Path) -> PathBuf {
    data_root.join("libs")
}

/// Path of the agent jar.
pub fn agent_path(data_root: &Path) -> PathBuf {
    agent_dir(data_root).join(AUTHLIB_INJECTOR_FILE)
}

/// Ensure the agent jar exists, downloading it when missing.
pub async fn ensure_agent(client: &reqwest::Client, data_root: &Path) -> Result<PathBuf> {
    let path = agent_path(data_root);
    if path.is_file() {
        return Ok(path);
    }

    std::fs::create_dir_all(agent_dir(data_root))?;
    info!(url = AUTHLIB_INJECTOR_URL, "downloading authlib-injector");

    let bytes = http::get_bytes(client, AUTHLIB_INJECTOR_URL).await?;
    // The upstream release publishes no stable checksum in a machine-readable
    // form, so validate the shape instead: a jar is a ZIP and must be plausible.
    if bytes.len() < 10_000 || !bytes.starts_with(b"PK") {
        return Err(MangoError::Other(
            "authlib-injector download did not look like a jar; not installing it".to_owned(),
        ));
    }

    tokio::fs::write(&path, &bytes).await?;
    Ok(path)
}

/// Build the `-javaagent` value for a custom auth server.
///
/// The base URL goes after `=`; authlib-injector appends the Yggdrasil paths
/// itself. A trailing slash is removed because the agent rejects it.
pub fn javaagent_argument(agent_jar: &Path, auth_server: &str) -> String {
    let base = auth_server.trim_end_matches('/');
    format!("-javaagent:{}={base}", agent_jar.display())
}

/// Build the prefetch property that saves the game a metadata round-trip.
///
/// authlib-injector accepts the server's metadata document base64-encoded, which
/// avoids a blocking fetch during game start. The value is a JVM property, so it
/// must not contain whitespace.
pub fn prefetched_property(metadata_json: &str) -> String {
    let encoded = STANDARD.encode(metadata_json.as_bytes());
    format!("-Dauthlibinjector.yggdrasil.prefetched={encoded}")
}

/// Whether a launch needs the agent.
pub fn is_required(yggdrasil_base: Option<&str>) -> bool {
    yggdrasil_base.is_some_and(|base| !base.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_path_is_under_the_data_root() {
        let root = Path::new("/data/mango");
        assert_eq!(agent_path(root), PathBuf::from("/data/mango/libs/authlib-injector.jar"));
    }

    #[test]
    fn javaagent_points_at_the_jar_and_server() {
        let argument = javaagent_argument(
            Path::new("/data/libs/authlib-injector.jar"),
            "https://auth.example.com/api/yggdrasil",
        );
        assert_eq!(
            argument,
            "-javaagent:/data/libs/authlib-injector.jar=https://auth.example.com/api/yggdrasil"
        );
    }

    #[test]
    fn a_trailing_slash_is_removed_from_the_server_url() {
        // authlib-injector rejects a base URL ending in '/'.
        let argument = javaagent_argument(Path::new("agent.jar"), "https://auth.example.com/");
        assert!(argument.ends_with("=https://auth.example.com"), "{argument}");
    }

    #[test]
    fn prefetched_metadata_is_base64_and_has_no_whitespace() {
        let property = prefetched_property(r#"{"meta":{"serverName":"Example"}}"#);
        assert!(property.starts_with("-Dauthlibinjector.yggdrasil.prefetched="));

        let encoded = property.split('=').nth(1).unwrap();
        assert!(!encoded.contains(char::is_whitespace), "{encoded}");

        // Round-trips back to the original JSON.
        let decoded = STANDARD.decode(encoded).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), r#"{"meta":{"serverName":"Example"}}"#);
    }

    #[test]
    fn the_agent_is_only_needed_for_a_custom_server() {
        assert!(is_required(Some("https://auth.example.com")));
        assert!(!is_required(None));
        assert!(!is_required(Some("   ")));
    }

    #[test]
    fn the_pinned_url_is_https_and_pinned() {
        assert!(AUTHLIB_INJECTOR_URL.starts_with("https://"));
        assert!(AUTHLIB_INJECTOR_URL.contains("v1.2.5"), "must pin a release");
    }
}
