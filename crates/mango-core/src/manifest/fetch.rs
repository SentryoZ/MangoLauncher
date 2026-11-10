//! Fetching a manifest over HTTP with an offline-capable cache.
//!
//! Manifests are small and change rarely, so a conditional request plus a local
//! cache gives both fast startups and the ability to launch while the manifest
//! host is down — the same guarantee Helios gets from
//! `refreshDistributionOrFallback`.

use std::path::{Path, PathBuf};

use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::error::{MangoError, Result};
use crate::manifest::Manifest;
use crate::net::http;

/// Cached manifest plus the validators needed for a conditional request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedManifest {
    etag: Option<String>,
    /// Raw manifest text, kept verbatim so parsing errors stay debuggable.
    body: String,
}

/// Where a manifest came from, for logs and UI messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManifestOrigin {
    Network,
    /// Server answered `304 Not Modified`.
    NotModified,
    /// Network failed; the cached copy was used.
    Cache,
    /// Read straight from a local file, bypassing HTTP.
    LocalFile,
}

#[derive(Debug, Clone)]
pub struct FetchedManifest {
    pub manifest: Manifest,
    pub origin: ManifestOrigin,
}

/// Load a manifest from a local path.
pub fn load_local(path: &Path) -> Result<FetchedManifest> {
    let text = std::fs::read_to_string(path)?;
    let manifest = Manifest::parse(&text)?;
    Ok(FetchedManifest { manifest, origin: ManifestOrigin::LocalFile })
}

/// Fetch a manifest, falling back to the cache when the network fails.
///
/// `allow_offline_fallback` exists so `--locked` installs can require a fresh
/// manifest when the caller cares about reproducibility.
pub async fn fetch(
    client: &Client,
    url: &str,
    cache_path: &Path,
    allow_offline_fallback: bool,
) -> Result<FetchedManifest> {
    let cached = read_cache(cache_path);

    let etag = cached.as_ref().and_then(|entry| entry.etag.as_deref());
    match http::get_text_with_etag(client, url, etag).await {
        Ok(response) => match response.body {
            None => {
                // 304: the cache is authoritative.
                let entry = cached.ok_or_else(|| {
                    MangoError::Other(format!("{url} returned 304 but no cached manifest exists"))
                })?;
                info!("manifest unchanged, using cache");
                Ok(FetchedManifest {
                    manifest: Manifest::parse(&entry.body)?,
                    origin: ManifestOrigin::NotModified,
                })
            }
            Some(body) => {
                let manifest = Manifest::parse(&body)?;
                write_cache(cache_path, &CachedManifest { etag: response.etag, body }).ok();
                info!(%url, "manifest downloaded");
                Ok(FetchedManifest { manifest, origin: ManifestOrigin::Network })
            }
        },
        Err(err) => {
            let Some(entry) = cached else { return Err(err) };
            if !allow_offline_fallback {
                return Err(MangoError::Other(format!(
                    "{err}; a cached manifest exists but offline fallback is disabled"
                )));
            }
            warn!(error = %err, "manifest fetch failed, falling back to cache");
            Ok(FetchedManifest {
                manifest: Manifest::parse(&entry.body)?,
                origin: ManifestOrigin::Cache,
            })
        }
    }
}

fn read_cache(path: &Path) -> Option<CachedManifest> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

fn write_cache(path: &Path, entry: &CachedManifest) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string(entry)?)?;
    Ok(())
}

/// Default cache location for a manifest URL.
pub fn cache_path_for(cache_root: &Path, url: &str) -> PathBuf {
    // Hash the URL so several manifests can coexist in one cache directory.
    let digest =
        crate::net::hash::hash_bytes(crate::net::hash::HashAlgorithm::Sha256, url.as_bytes());
    cache_root.join(format!("manifest-{}.json", &digest[..16]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_paths_are_stable_and_distinct() {
        let root = Path::new("/cache");
        let a = cache_path_for(root, "https://example.com/a.json");
        let b = cache_path_for(root, "https://example.com/b.json");
        assert_ne!(a, b);
        assert_eq!(a, cache_path_for(root, "https://example.com/a.json"));
        assert!(a.starts_with(root));
    }

    #[test]
    fn local_manifest_is_parsed() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/manifest.json");
        let fetched = load_local(&path).unwrap();
        assert_eq!(fetched.origin, ManifestOrigin::LocalFile);
        assert_eq!(fetched.manifest.name, "Mango Network");
    }

    #[test]
    fn missing_local_manifest_is_an_error() {
        assert!(load_local(Path::new("/definitely/missing/manifest.json")).is_err());
    }

    #[test]
    fn cache_write_and_read_round_trip() {
        let dir = std::env::temp_dir().join(format!("mango-mcache-{}", std::process::id()));
        let path = dir.join("manifest-cache.json");
        let entry = CachedManifest { etag: Some("\"abc\"".to_owned()), body: "{}".to_owned() };

        write_cache(&path, &entry).unwrap();
        assert_eq!(read_cache(&path), Some(entry));

        std::fs::remove_dir_all(&dir).ok();
    }
}
