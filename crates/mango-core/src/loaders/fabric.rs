//! Fabric loader resolution via the Fabric Meta API.
//!
//! `GET /v2/versions/loader/{game}/{loader}/profile/json` returns a ready
//! `version.json` with `inheritsFrom` pointing at the vanilla version, so the
//! launcher only has to download it and merge.

use serde::Deserialize;
use tracing::info;

use crate::error::{MangoError, Result};
use crate::loaders::LoaderProfile;
use crate::mojang::VersionJson;
use crate::net::http;

pub const FABRIC_META: &str = "https://meta.fabricmc.net/v2";

#[derive(Debug, Deserialize)]
struct LoaderEntry {
    loader: LoaderInfo,
}

#[derive(Debug, Deserialize)]
struct LoaderInfo {
    version: String,
    #[serde(default)]
    stable: bool,
}

/// Newest loader version for a game version.
///
/// Prefers a stable release; falls back to the newest build so bleeding-edge
/// game versions still resolve.
pub async fn latest_loader_version(client: &reqwest::Client, minecraft: &str) -> Result<String> {
    let url = format!("{FABRIC_META}/versions/loader/{minecraft}");
    let entries: Vec<LoaderEntry> = http::get_json(client, &url).await?;

    entries
        .iter()
        .find(|entry| entry.loader.stable)
        .or_else(|| entries.first())
        .map(|entry| entry.loader.version.clone())
        .ok_or_else(|| MangoError::Other(format!("Fabric has no loader for Minecraft {minecraft}")))
}

/// Every loader version offered for a game version, newest first.
pub async fn version_list(client: &reqwest::Client, minecraft: &str) -> Result<Vec<String>> {
    let url = format!("{FABRIC_META}/versions/loader/{minecraft}");
    let entries: Vec<LoaderEntry> = http::get_json(client, &url).await?;
    Ok(entries.into_iter().map(|entry| entry.loader.version).collect())
}

/// Fetch the loader profile for a game version.
pub async fn resolve(
    client: &reqwest::Client,
    minecraft: &str,
    version: Option<&str>,
) -> Result<LoaderProfile> {
    let loader_version = match version {
        Some(version) => version.to_owned(),
        None => latest_loader_version(client, minecraft).await?,
    };

    let url = format!("{FABRIC_META}/versions/loader/{minecraft}/{loader_version}/profile/json");
    info!(%url, "fetching fabric profile");

    let bytes = http::get_bytes(client, &url).await?;
    let profile = VersionJson::parse(&bytes)?;

    Ok(LoaderProfile {
        version_id: profile.id.clone(),
        jvm_args: Vec::new(),
        game_args: Vec::new(),
        profile,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_url_is_https() {
        assert!(FABRIC_META.starts_with("https://"));
    }

    #[test]
    fn loader_list_prefers_stable() {
        // Mirrors the exact shape the meta API returns.
        let entries: Vec<LoaderEntry> = serde_json::from_str(
            r#"[{"loader":{"version":"0.17.0-beta.1","stable":false}},
                {"loader":{"version":"0.16.9","stable":true}}]"#,
        )
        .unwrap();
        let chosen = entries
            .iter()
            .find(|entry| entry.loader.stable)
            .or_else(|| entries.first())
            .map(|entry| entry.loader.version.clone());
        assert_eq!(chosen.as_deref(), Some("0.16.9"));
    }

    #[test]
    fn loader_list_falls_back_to_first_entry() {
        let entries: Vec<LoaderEntry> =
            serde_json::from_str(r#"[{"loader":{"version":"0.17.0-beta.1","stable":false}}]"#)
                .unwrap();
        let chosen = entries
            .iter()
            .find(|entry| entry.loader.stable)
            .or_else(|| entries.first())
            .map(|entry| entry.loader.version.clone());
        assert_eq!(chosen.as_deref(), Some("0.17.0-beta.1"));
    }

    #[test]
    fn empty_loader_list_resolves_to_none() {
        let entries: Vec<LoaderEntry> = serde_json::from_str("[]").unwrap();
        assert!(
            entries.iter().find(|entry| entry.loader.stable).or_else(|| entries.first()).is_none()
        );
    }

    #[test]
    fn entries_ignore_extra_metadata_fields() {
        // The real response also carries `intermediary` and `launcherMeta`.
        let entries: Vec<LoaderEntry> = serde_json::from_str(
            r#"[{"loader":{"separator":".","build":1,"maven":"net.fabricmc:fabric-loader:0.20.1",
                          "version":"0.20.1","stable":true},
                 "intermediary":{"version":"1.20.1"},
                 "launcherMeta":{"version":2}}]"#,
        )
        .unwrap();
        assert_eq!(entries[0].loader.version, "0.20.1");
        assert!(entries[0].loader.stable);
    }
}
