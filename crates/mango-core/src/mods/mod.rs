//! Resolving mods from upstream catalogues.
//!
//! A manifest lists mods by project id, not by file: the launcher asks the
//! catalogue for a version matching the instance's game version and loader, then
//! downloads the file from the catalogue's CDN. That keeps the manifest small and
//! means mod updates do not require republishing the manifest, while the lockfile
//! records exactly which file was chosen so an install stays reproducible.

pub mod curseforge;
pub mod modrinth;

use crate::error::{MangoError, Result};
use crate::manifest::{LoaderKind, ModEntry, ModSource};
use crate::net::downloader::FileHash;

/// A mod file that has been pinned to a concrete upstream version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedMod {
    pub source: ModSource,
    /// Catalogue project id (Modrinth's opaque id, or CurseForge's number).
    pub project_id: String,
    /// Catalogue version id, recorded in the lockfile.
    pub version_id: String,
    /// Human-readable version, e.g. `mc1.20.1-0.5.13`.
    pub version_number: String,
    pub file_name: String,
    pub url: String,
    pub hash: Option<FileHash>,
    pub size: u64,
    pub required: bool,
}

/// Modrinth's loader names, which differ from our enum spellings.
pub fn modrinth_loader(kind: LoaderKind) -> Option<&'static str> {
    match kind {
        LoaderKind::Fabric => Some("fabric"),
        LoaderKind::Quilt => Some("quilt"),
        LoaderKind::Forge => Some("forge"),
        LoaderKind::Neoforge => Some("neoforge"),
        // Vanilla has no mod loader, so nothing can be resolved.
        LoaderKind::Vanilla => None,
    }
}

/// CurseForge's numeric mod-loader ids.
pub fn curseforge_loader(kind: LoaderKind) -> Option<u32> {
    match kind {
        LoaderKind::Forge => Some(1),
        LoaderKind::Fabric => Some(4),
        LoaderKind::Quilt => Some(5),
        LoaderKind::Neoforge => Some(6),
        LoaderKind::Vanilla => None,
    }
}

/// Resolve every mod entry for one instance.
///
/// Entries that cannot be resolved are reported together rather than aborting on
/// the first failure, so the player sees every problem at once. Optional mods
/// that fail are downgraded to warnings; required mods fail the install.
pub async fn resolve_all(
    client: &reqwest::Client,
    entries: &[ModEntry],
    minecraft: &str,
    loader: LoaderKind,
    curseforge_key: Option<&str>,
) -> Result<ResolveReport> {
    let mut resolved = Vec::new();
    let mut failures = Vec::new();

    for entry in entries {
        let outcome = match entry.source {
            ModSource::Modrinth => modrinth::resolve(client, entry, minecraft, loader).await,
            ModSource::Curseforge => {
                curseforge::resolve(client, entry, minecraft, loader, curseforge_key).await
            }
        };

        match outcome {
            Ok(Some(found)) => resolved.push(found),
            Ok(None) => failures.push(Failure {
                id: entry.id.clone(),
                required: entry.required,
                reason: "no matching version".to_owned(),
            }),
            Err(err) => failures.push(Failure {
                id: entry.id.clone(),
                required: entry.required,
                reason: err.to_string(),
            }),
        }
    }

    let blocking: Vec<_> = failures.iter().filter(|failure| failure.required).collect();
    if !blocking.is_empty() {
        let detail = blocking
            .iter()
            .map(|failure| format!("{} ({})", failure.id, failure.reason))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(MangoError::Other(format!("could not resolve required mods: {detail}")));
    }

    Ok(ResolveReport { resolved, failures })
}

/// Outcome of resolving a manifest's mods.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolveReport {
    pub resolved: Vec<ResolvedMod>,
    /// Optional mods that could not be resolved, kept for the UI to display.
    pub failures: Vec<Failure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub id: String,
    pub required: bool,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loader_names_match_each_catalogue() {
        assert_eq!(modrinth_loader(LoaderKind::Fabric), Some("fabric"));
        assert_eq!(modrinth_loader(LoaderKind::Quilt), Some("quilt"));
        assert_eq!(modrinth_loader(LoaderKind::Neoforge), Some("neoforge"));
        assert_eq!(modrinth_loader(LoaderKind::Vanilla), None);

        assert_eq!(curseforge_loader(LoaderKind::Forge), Some(1));
        assert_eq!(curseforge_loader(LoaderKind::Fabric), Some(4));
        assert_eq!(curseforge_loader(LoaderKind::Vanilla), None);
    }

    #[tokio::test]
    async fn required_failures_abort_the_install() {
        // An unknown Modrinth project cannot resolve, and it is required.
        let client = crate::net::http::build_client().unwrap();
        let entries = vec![ModEntry {
            source: ModSource::Modrinth,
            id: "this-project-definitely-does-not-exist-xyz".to_owned(),
            version: None,
            file_id: None,
            required: true,
            disabled_by_default: false,
        }];

        let error =
            resolve_all(&client, &entries, "1.20.1", LoaderKind::Fabric, None).await.unwrap_err();
        assert!(error.to_string().contains("required mods"), "{error}");
    }

    #[tokio::test]
    async fn optional_failures_are_reported_not_fatal() {
        let client = crate::net::http::build_client().unwrap();
        let entries = vec![ModEntry {
            source: ModSource::Modrinth,
            id: "this-project-definitely-does-not-exist-xyz".to_owned(),
            version: None,
            file_id: None,
            required: false,
            disabled_by_default: false,
        }];

        let report =
            resolve_all(&client, &entries, "1.20.1", LoaderKind::Fabric, None).await.unwrap();
        assert!(report.resolved.is_empty());
        assert_eq!(report.failures.len(), 1);
        assert!(!report.failures[0].required);
    }

    #[tokio::test]
    async fn curseforge_without_a_key_degrades_gracefully() {
        let client = crate::net::http::build_client().unwrap();
        let entries = vec![ModEntry {
            source: ModSource::Curseforge,
            id: "238222".to_owned(),
            version: None,
            file_id: None,
            required: false,
            disabled_by_default: false,
        }];

        let report =
            resolve_all(&client, &entries, "1.20.1", LoaderKind::Forge, None).await.unwrap();
        assert!(report.resolved.is_empty());
        assert!(report.failures[0].reason.contains("API key"));
    }
}
