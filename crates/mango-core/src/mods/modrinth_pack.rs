//! Browsing and downloading Modrinth modpacks.
//!
//! A modpack is a project whose versions ship a `.mrpack` archive. The launcher
//! only needs search and the version list: what to install is decided by the
//! archive's own index, not by resolving dependencies here.

use serde::Deserialize;
use tracing::debug;

use crate::error::{MangoError, Result};
use crate::net::http;

use super::modpack::Modpack;

const MODRINTH_API: &str = "https://api.modrinth.com/v2";

/// How many results one search request returns.
///
/// Modrinth caps this at 100; 20 fills a scrollable list without making the first
/// paint wait on a large payload.
pub const SEARCH_LIMIT: u32 = 20;

/// One modpack in a search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackSummary {
    /// URL slug, which is also the project id used in links.
    pub slug: String,
    pub title: String,
    pub description: String,
    pub icon_url: Option<String>,
    pub downloads: u64,
    /// Newest game versions the pack supports, for display.
    pub game_versions: Vec<String>,
}

/// One installable version of a pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackVersion {
    pub id: String,
    pub version_number: String,
    pub game_versions: Vec<String>,
    /// Loader names as Modrinth spells them, e.g. `fabric`.
    pub loaders: Vec<String>,
    /// The `.mrpack` download.
    pub url: String,
    pub file_name: String,
    pub size: u64,
    /// `release`, `beta`, or `alpha`.
    pub version_type: String,
}

impl PackVersion {
    /// Whether this is a stable release.
    pub fn is_release(&self) -> bool {
        self.version_type == "release"
    }
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    hits: Vec<SearchHit>,
}

#[derive(Debug, Deserialize)]
struct SearchHit {
    slug: String,
    title: String,
    #[serde(default)]
    description: String,
    #[serde(rename = "icon_url", default)]
    icon_url: Option<String>,
    #[serde(default)]
    downloads: u64,
    #[serde(rename = "versions", default)]
    game_versions: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct Version {
    id: String,
    #[serde(default)]
    version_number: String,
    #[serde(default)]
    version_type: String,
    #[serde(default)]
    game_versions: Vec<String>,
    #[serde(default)]
    loaders: Vec<String>,
    #[serde(default)]
    files: Vec<VersionFile>,
}

#[derive(Debug, Deserialize)]
struct VersionFile {
    url: String,
    #[serde(default)]
    filename: String,
    #[serde(default)]
    size: u64,
}

/// Search modpacks.
///
/// `query` may be empty, which returns the most downloaded packs.
pub async fn search(
    client: &reqwest::Client,
    query: &str,
    game_version: Option<&str>,
    limit: u32,
) -> Result<Vec<PackSummary>> {
    let mut facets = vec![r#"["project_type:modpack"]"#.to_owned()];

    if let Some(version) = game_version.filter(|version| !version.trim().is_empty()) {
        facets.push(format!(r#"["versions:{}"]"#, version));
    }

    let facet_json = format!("[{}]", facets.join(","));
    let url = format!(
        "{MODRINTH_API}/search?query={}&limit={}&index=downloads&facets={}",
        encode(query),
        limit.clamp(1, 100),
        encode(&facet_json)
    );

    let response: SearchResponse = http::get_json(client, &url).await?;
    debug!(query, count = response.hits.len(), "modpack search");

    Ok(response
        .hits
        .into_iter()
        .map(|hit| PackSummary {
            slug: hit.slug,
            title: hit.title,
            description: hit.description,
            icon_url: hit.icon_url,
            downloads: hit.downloads,
            game_versions: hit.game_versions,
        })
        .collect())
}

/// Every installable version of a pack, newest first.
pub async fn versions(client: &reqwest::Client, project: &str) -> Result<Vec<PackVersion>> {
    let url = format!("{MODRINTH_API}/project/{}/version", encode(project));

    let versions: Vec<Version> = http::get_json(client, &url).await?;
    debug!(project, count = versions.len(), "modpack versions");

    Ok(versions
        .into_iter()
        .filter_map(|version| {
            // Only a `.mrpack` can be installed; a pack may also publish a
            // CurseForge-style archive or nothing at all.
            let file = version.files.iter().find(|file| file.filename.ends_with(".mrpack"))?;
            Some(PackVersion {
                id: version.id,
                version_number: version.version_number,
                game_versions: version.game_versions,
                loaders: version.loaders,
                url: file.url.clone(),
                file_name: file.filename.clone(),
                size: file.size,
                version_type: version.version_type,
            })
        })
        .collect())
}

/// Pick the version a player most likely wants.
///
/// Prefers a stable release for the requested game version; falls back to the
/// newest anything, because a pack that only ships betas is still installable.
pub fn preferred_version<'a>(
    versions: &'a [PackVersion],
    game_version: Option<&str>,
) -> Option<&'a PackVersion> {
    let matches_game = |version: &&PackVersion| match game_version {
        Some(wanted) if !wanted.trim().is_empty() => {
            version.game_versions.iter().any(|version| version == wanted)
        }
        _ => true,
    };

    let releases: Vec<&PackVersion> =
        versions.iter().filter(matches_game).filter(|version| version.is_release()).collect();

    if let Some(release) = releases.first() {
        return Some(release);
    }

    versions.iter().find(matches_game).or_else(|| versions.first())
}

/// Download an archive and read it.
///
/// The archive is kept in memory only: a pack's index is small, and the
/// downloaded files themselves are streamed by the installer afterwards. The bytes
/// of `overrides/` do live here, which is why a pack is capped.
pub async fn download(client: &reqwest::Client, version: &PackVersion) -> Result<Modpack> {
    /// A pack that ships more than this is almost certainly a mistake or an
    /// attack; the largest real packs are a few hundred megabytes of overrides.
    const MAX_PACK_BYTES: u64 = 512 * 1024 * 1024;

    if version.size > MAX_PACK_BYTES {
        return Err(MangoError::Other(format!(
            "the pack archive is {} MB, which is larger than the launcher will read",
            version.size / (1024 * 1024)
        )));
    }

    let bytes = http::get_bytes(client, &version.url).await?;
    if bytes.len() as u64 > MAX_PACK_BYTES {
        return Err(MangoError::Other("the pack archive was unexpectedly large".to_owned()));
    }

    Modpack::read(std::io::Cursor::new(bytes))
}

/// Percent-encode a query component.
fn encode(value: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn version(id: &str, kind: &str, games: &[&str]) -> PackVersion {
        PackVersion {
            id: id.to_owned(),
            version_number: id.to_owned(),
            game_versions: games.iter().map(|game| (*game).to_owned()).collect(),
            loaders: vec!["fabric".to_owned()],
            url: format!("https://cdn/{id}.mrpack"),
            file_name: format!("{id}.mrpack"),
            size: 1000,
            version_type: kind.to_owned(),
        }
    }

    #[test]
    fn a_stable_release_for_the_wanted_version_wins() {
        let versions = vec![
            version("newest-beta", "beta", &["1.20.1"]),
            version("old-release", "release", &["1.19.2"]),
            version("right-release", "release", &["1.20.1"]),
        ];
        let picked = preferred_version(&versions, Some("1.20.1")).unwrap();
        assert_eq!(picked.id, "right-release");
    }

    #[test]
    fn a_beta_is_used_when_the_pack_ships_no_release_for_that_version() {
        let versions = vec![
            version("other-release", "release", &["1.19.2"]),
            version("beta", "beta", &["1.20.1"]),
        ];
        assert_eq!(preferred_version(&versions, Some("1.20.1")).unwrap().id, "beta");
    }

    #[test]
    fn without_a_wanted_version_the_newest_release_wins() {
        let versions =
            vec![version("beta", "beta", &["1.21"]), version("release", "release", &["1.20.1"])];
        assert_eq!(preferred_version(&versions, None).unwrap().id, "release");
    }

    #[test]
    fn an_empty_list_picks_nothing() {
        assert!(preferred_version(&[], Some("1.20.1")).is_none());
    }

    #[test]
    fn a_version_with_no_matching_game_falls_back_rather_than_failing() {
        // Nothing matches, but the pack is still installable on the version it
        // targets, so the player gets it instead of a dead button.
        let versions = vec![version("only", "release", &["1.19.2"])];
        assert_eq!(preferred_version(&versions, Some("1.20.1")).unwrap().id, "only");
    }

    #[test]
    fn the_archive_must_be_a_mrpack() {
        // A version can ship other file types; only `.mrpack` is installable.
        let json = r#"[{"id":"v1","version_number":"1","version_type":"release",
            "game_versions":["1.20.1"],"loaders":["fabric"],
            "files":[{"url":"https://cdn/pack.zip","filename":"pack.zip","size":10}]}]"#;
        let versions: Vec<Version> = serde_json::from_str(json).unwrap();

        let usable: Vec<&VersionFile> =
            versions[0].files.iter().filter(|file| file.filename.ends_with(".mrpack")).collect();
        assert!(usable.is_empty(), "a .zip is not a Modrinth pack");
    }

    #[test]
    fn query_values_are_percent_encoded() {
        assert_eq!(encode("fabulously optimized"), "fabulously%20optimized");
        assert_eq!(encode(r#"["project_type:modpack"]"#), "%5B%22project_type%3Amodpack%22%5D");
    }
}
