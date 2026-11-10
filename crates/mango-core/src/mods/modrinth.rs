//! Modrinth mod resolution.
//!
//! Modrinth needs no API key, only a descriptive `User-Agent`, so it is the
//! default catalogue.

use serde::Deserialize;
use tracing::debug;

use crate::error::{MangoError, Result};
use crate::manifest::{LoaderKind, ModEntry};
use crate::mods::{ResolvedMod, modrinth_loader};
use crate::net::downloader::FileHash;
use crate::net::http;

pub const MODRINTH_API: &str = "https://api.modrinth.com/v2";

#[derive(Debug, Deserialize)]
struct Version {
    id: String,
    #[serde(default)]
    version_number: String,
    #[serde(default)]
    version_type: String,
    files: Vec<VersionFile>,
}

#[derive(Debug, Deserialize)]
struct VersionFile {
    #[serde(default)]
    filename: String,
    url: String,
    #[serde(default)]
    primary: bool,
    #[serde(default)]
    size: u64,
    #[serde(default)]
    hashes: Hashes,
}

#[derive(Debug, Default, Deserialize)]
struct Hashes {
    #[serde(default)]
    sha512: Option<String>,
    #[serde(default)]
    sha1: Option<String>,
}

/// Resolve one entry, or `Ok(None)` when no version matches.
pub async fn resolve(
    client: &reqwest::Client,
    entry: &ModEntry,
    minecraft: &str,
    loader: LoaderKind,
) -> Result<Option<ResolvedMod>> {
    let Some(loader_name) = modrinth_loader(loader) else {
        return Ok(None);
    };

    let game_version = serde_json::to_string(&[minecraft]).map_err(MangoError::from)?;
    let loader_filter = serde_json::to_string(&[loader_name]).map_err(MangoError::from)?;
    let url = format!(
        "{MODRINTH_API}/project/{}/version?game_versions={}&loaders={}",
        encode(&entry.id),
        encode(&game_version),
        encode(&loader_filter)
    );

    let versions: Vec<Version> = http::get_json(client, &url).await?;
    debug!(project = %entry.id, count = versions.len(), "modrinth versions");

    let picked = match &entry.version {
        // A specific version id, version number, or `latest`.
        Some(wanted) if wanted != "latest" => versions
            .iter()
            .find(|version| version.id == *wanted || version.version_number == *wanted),
        _ => pick_preferred(&versions),
    };

    let Some(version) = picked else { return Ok(None) };

    // Cargo-cult protection: prefer the file the author marked primary, because
    // a version can ship sources alongside the distributable jar.
    let Some(file) =
        version.files.iter().find(|file| file.primary).or_else(|| version.files.first())
    else {
        return Ok(None);
    };

    let hash = file
        .hashes
        .sha512
        .clone()
        .map(FileHash::sha512)
        .or_else(|| file.hashes.sha1.clone().map(FileHash::sha1));

    Ok(Some(ResolvedMod {
        source: crate::manifest::ModSource::Modrinth,
        project_id: entry.id.clone(),
        version_id: version.id.clone(),
        version_number: version.version_number.clone(),
        file_name: file.filename.clone(),
        url: file.url.clone(),
        hash,
        size: file.size,
        required: entry.required,
    }))
}

/// Choose the version a player would expect: newest stable, else newest.
///
/// Modrinth returns versions newest-first, so the first match wins.
fn pick_preferred(versions: &[Version]) -> Option<&Version> {
    versions.iter().find(|version| version.version_type == "release").or_else(|| versions.first())
}

/// Percent-encode a query component without pulling in a URL crate.
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

    const VERSIONS: &str = r#"[
        {"id":"beta1","version_number":"0.6.0-beta","version_type":"beta",
         "files":[{"filename":"beta.jar","url":"https://cdn/beta.jar","primary":true,"size":10,
                   "hashes":{"sha512":"aaaa","sha1":"bbbb"}}]},
        {"id":"rel1","version_number":"0.5.13","version_type":"release",
         "files":[{"filename":"sources.jar","url":"https://cdn/src.jar","primary":false,"size":1,"hashes":{}},
                  {"filename":"sodium.jar","url":"https://cdn/sodium.jar","primary":true,"size":971552,
                   "hashes":{"sha512":"cccc","sha1":"dddd"}}]}
    ]"#;

    #[test]
    fn response_shape_parses() {
        let versions: Vec<Version> = serde_json::from_str(VERSIONS).unwrap();
        assert_eq!(versions.len(), 2);
        assert_eq!(versions[1].files.len(), 2);
        assert_eq!(versions[1].files[1].hashes.sha512.as_deref(), Some("cccc"));
    }

    #[test]
    fn release_is_preferred_over_beta() {
        let versions: Vec<Version> = serde_json::from_str(VERSIONS).unwrap();
        assert_eq!(pick_preferred(&versions).unwrap().id, "rel1");
    }

    #[test]
    fn falls_back_to_newest_when_no_release() {
        let only_beta: Vec<Version> = serde_json::from_str(
            r#"[{"id":"b","version_number":"1","version_type":"beta","files":[]}]"#,
        )
        .unwrap();
        assert_eq!(pick_preferred(&only_beta).unwrap().id, "b");
    }

    #[test]
    fn primary_file_is_chosen_over_sources() {
        let versions: Vec<Version> = serde_json::from_str(VERSIONS).unwrap();
        let version = pick_preferred(&versions).unwrap();
        let file = version
            .files
            .iter()
            .find(|file| file.primary)
            .or_else(|| version.files.first())
            .unwrap();
        assert_eq!(file.filename, "sodium.jar");
        assert_eq!(file.hashes.sha512.as_deref(), Some("cccc"));
    }

    #[test]
    fn falls_back_to_first_file_without_a_primary() {
        let version: Version = serde_json::from_str(
            r#"{"id":"x","version_number":"1","version_type":"release",
                "files":[{"filename":"a.jar","url":"https://cdn/a.jar","size":5}]}"#,
        )
        .unwrap();
        let file = version
            .files
            .iter()
            .find(|file| file.primary)
            .or_else(|| version.files.first())
            .unwrap();
        assert_eq!(file.filename, "a.jar");
    }

    #[test]
    fn query_values_are_percent_encoded() {
        // The bracket quoting Modrinth requires must survive encoding.
        assert_eq!(encode(r#"["1.20.1"]"#), "%5B%221.20.1%22%5D");
        assert_eq!(encode("sodium"), "sodium");
    }
}
