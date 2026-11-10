//! Listing the loader versions available for a Minecraft release.
//!
//! The picker in the UI and `mango instance` both need this, and each loader
//! exposes the information differently, so the differences stay here rather than
//! leaking into callers.

use crate::error::Result;
use crate::manifest::LoaderKind;

/// Available loader versions for a game version, newest first.
///
/// Vanilla has no versions to choose from and returns an empty list. An empty
/// result for a modded loader means that loader does not support the release.
pub async fn available(
    client: &reqwest::Client,
    kind: LoaderKind,
    minecraft: &str,
) -> Result<Vec<String>> {
    match kind {
        LoaderKind::Vanilla => Ok(Vec::new()),
        LoaderKind::Fabric => super::fabric::version_list(client, minecraft).await,
        LoaderKind::Quilt => super::quilt::version_list(client, minecraft).await,
        LoaderKind::Forge => super::forge::version_list(client, minecraft).await,
        LoaderKind::Neoforge => super::forge::neoforge_version_list(client, minecraft).await,
    }
}

/// Whether a specific loader version is offered for a release.
///
/// Used to reject a hand-typed version before an install fails halfway through.
pub async fn supports(
    client: &reqwest::Client,
    kind: LoaderKind,
    minecraft: &str,
    version: &str,
) -> Result<bool> {
    // Vanilla has no version to validate.
    if kind == LoaderKind::Vanilla {
        return Ok(version.trim().is_empty());
    }
    Ok(available(client, kind, minecraft).await?.iter().any(|known| known == version))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn vanilla_has_no_loader_versions() {
        let client = crate::net::http::build_client().unwrap();
        assert!(available(&client, LoaderKind::Vanilla, "1.20.1").await.unwrap().is_empty());
        assert!(supports(&client, LoaderKind::Vanilla, "1.20.1", "").await.unwrap());
        assert!(!supports(&client, LoaderKind::Vanilla, "1.20.1", "1.0").await.unwrap());
    }
}
