//! Quilt loader resolution.
//!
//! Quilt exposes the same profile endpoint as Fabric under its own meta host,
//! so the two modules stay deliberately similar.

use serde::Deserialize;
use tracing::info;

use crate::error::{MangoError, Result};
use crate::loaders::LoaderProfile;
use crate::mojang::VersionJson;
use crate::net::http;

pub const QUILT_META: &str = "https://meta.quiltmc.org/v3";

#[derive(Debug, Deserialize)]
struct LoaderEntry {
    /// Quilt nests the loader metadata the same way Fabric does.
    loader: LoaderInfo,
}

#[derive(Debug, Deserialize)]
struct LoaderInfo {
    version: String,
}

/// Newest loader version for a game version.
pub async fn latest_loader_version(client: &reqwest::Client, minecraft: &str) -> Result<String> {
    version_list(client, minecraft)
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| MangoError::Other(format!("Quilt has no loader for Minecraft {minecraft}")))
}

/// Every loader version offered for a game version, newest first.
pub async fn version_list(client: &reqwest::Client, minecraft: &str) -> Result<Vec<String>> {
    let url = format!("{QUILT_META}/versions/loader/{minecraft}");
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

    let url = format!("{QUILT_META}/versions/loader/{minecraft}/{loader_version}/profile/json");
    info!(%url, "fetching quilt profile");

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
        assert!(QUILT_META.starts_with("https://"));
    }

    #[test]
    fn loader_entries_parse_the_nested_shape() {
        // The real response nests version under `loader`, like Fabric's does.
        let entries: Vec<LoaderEntry> = serde_json::from_str(
            r#"[{"loader":{"maven":"org.quiltmc:quilt-loader:0.20.0-beta.9",
                          "version":"0.20.0-beta.9","build":9},
                 "hashed":{"version":"1.20.1"}}]"#,
        )
        .unwrap();
        assert_eq!(entries[0].loader.version, "0.20.0-beta.9");
    }
}
