//! CurseForge mod resolution.
//!
//! Unlike Modrinth, CurseForge requires an API key that is only issued after a
//! manual application review, and file downloads from their CDN are
//! key-authenticated too. Without a key every call returns `403`, so this module
//! reports a clear, actionable error instead of a bare HTTP failure.

use serde::Deserialize;
use tracing::debug;

use crate::error::{MangoError, Result};
use crate::manifest::{LoaderKind, ModEntry};
use crate::mods::{ResolvedMod, curseforge_loader};
use crate::net::downloader::FileHash;
use crate::net::http;

pub const CURSEFORGE_API: &str = "https://api.curseforge.com/v1";

/// Sent when no key is configured, so the caller can detect the condition.
pub const MISSING_KEY_MESSAGE: &str =
    "CurseForge requires an API key; set `curseforge.apiKey` in the launcher config";

#[derive(Debug, Deserialize)]
struct FilesResponse {
    data: Vec<File>,
}

#[derive(Debug, Deserialize)]
struct File {
    id: u64,
    #[serde(rename = "fileName", default)]
    file_name: String,
    #[serde(rename = "downloadUrl", default)]
    download_url: Option<String>,
    #[serde(rename = "fileLength", default)]
    file_length: u64,
    #[serde(rename = "fileDate", default)]
    file_date: Option<String>,
    #[serde(default)]
    hashes: Vec<FileHashEntry>,
}

#[derive(Debug, Deserialize)]
struct FileHashEntry {
    #[serde(default)]
    value: String,
    #[serde(default)]
    algo: u32,
}

/// Resolve one entry, or `Ok(None)` when no file matches.
pub async fn resolve(
    client: &reqwest::Client,
    entry: &ModEntry,
    minecraft: &str,
    loader: LoaderKind,
    api_key: Option<&str>,
) -> Result<Option<ResolvedMod>> {
    let Some(api_key) = api_key.filter(|key| !key.trim().is_empty()) else {
        return Err(MangoError::Other(MISSING_KEY_MESSAGE.to_owned()));
    };
    let Some(loader_id) = curseforge_loader(loader) else {
        return Ok(None);
    };

    // A pinned file id skips the listing entirely, which is the only reliable way
    // to reproduce an exact pack.
    if let Some(file_id) = entry.file_id {
        let url = format!("{CURSEFORGE_API}/mods/{}/files/{file_id}", entry.id);
        let response: SingleFileResponse =
            http::get_json_with_header(client, &url, "x-api-key", api_key).await?;
        return Ok(file_to_resolved(entry, &response.data));
    }

    let url = format!(
        "{CURSEFORGE_API}/mods/{}/files?gameVersion={}&modLoaderType={loader_id}",
        encode(&entry.id),
        encode(minecraft)
    );
    let response: FilesResponse =
        http::get_json_with_header(client, &url, "x-api-key", api_key).await?;

    debug!(project = %entry.id, count = response.data.len(), "curseforge files");

    let chosen = match &entry.version {
        Some(wanted) if wanted != "latest" => response.data.iter().find(|file| {
            file.file_name.contains(wanted.as_str()) || file.id.to_string() == *wanted
        }),
        _ => pick_newest(&response.data),
    };

    Ok(chosen.and_then(|file| file_to_resolved(entry, file)))
}

#[derive(Debug, Deserialize)]
struct SingleFileResponse {
    data: File,
}

fn pick_newest(files: &[File]) -> Option<&File> {
    // CurseForge lists files oldest-first for the listing endpoint.
    files.iter().max_by(|a, b| {
        let key = |file: &File| file.file_date.clone().unwrap_or_default();
        key(a).cmp(&key(b))
    })
}

fn file_to_resolved(entry: &ModEntry, file: &File) -> Option<ResolvedMod> {
    // Files that the author disallowed redistribution return no download URL.
    let url = file.download_url.clone()?;

    // algo 1 = SHA-1, algo 2 = MD5. Prefer SHA-1 when both are present.
    let hash = file
        .hashes
        .iter()
        .find(|entry| entry.algo == 1)
        .map(|entry| FileHash::sha1(entry.value.clone()));

    Some(ResolvedMod {
        source: crate::manifest::ModSource::Curseforge,
        project_id: entry.id.clone(),
        version_id: file.id.to_string(),
        version_number: file.file_name.clone(),
        file_name: file.file_name.clone(),
        url,
        hash,
        size: file.file_length,
        required: entry.required,
    })
}

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

    #[tokio::test]
    async fn missing_key_is_reported_clearly() {
        let client = http::build_client().unwrap();
        let entry = ModEntry {
            source: crate::manifest::ModSource::Curseforge,
            id: "238222".to_owned(),
            version: None,
            file_id: None,
            required: true,
            disabled_by_default: false,
        };

        let error = resolve(&client, &entry, "1.20.1", LoaderKind::Forge, None).await.unwrap_err();
        assert!(error.to_string().contains("API key"), "{error}");
    }

    #[test]
    fn newest_file_is_selected_by_date() {
        let files: Vec<File> = serde_json::from_str(
            r#"[
                {"id":1,"fileName":"old.jar","fileDate":"2023-01-01T00:00:00Z","hashes":[]},
                {"id":2,"fileName":"new.jar","fileDate":"2024-06-01T00:00:00Z","hashes":[]}
            ]"#,
        )
        .unwrap();
        assert_eq!(pick_newest(&files).unwrap().id, 2);
    }

    #[test]
    fn files_without_download_urls_are_skipped() {
        let entry = ModEntry {
            source: crate::manifest::ModSource::Curseforge,
            id: "1".to_owned(),
            version: None,
            file_id: None,
            required: true,
            disabled_by_default: false,
        };
        let file: File = serde_json::from_str(
            r#"{"id":9,"fileName":"x.jar","downloadUrl":null,"fileLength":1,"hashes":[]}"#,
        )
        .unwrap();
        assert!(file_to_resolved(&entry, &file).is_none());
    }

    #[test]
    fn sha1_hash_is_preferred_over_md5() {
        let entry = ModEntry {
            source: crate::manifest::ModSource::Curseforge,
            id: "1".to_owned(),
            version: None,
            file_id: None,
            required: true,
            disabled_by_default: false,
        };
        let file: File = serde_json::from_str(
            r#"{"id":9,"fileName":"x.jar","downloadUrl":"https://edge/x.jar","fileLength":5,
                "hashes":[{"value":"md5val","algo":2},{"value":"sha1val","algo":1}]}"#,
        )
        .unwrap();

        let resolved = file_to_resolved(&entry, &file).unwrap();
        assert_eq!(resolved.hash.unwrap().value, "sha1val");
    }

    #[test]
    fn listing_response_parses() {
        let response: FilesResponse = serde_json::from_str(
            r#"{"data":[{"id":1,"fileName":"a.jar","downloadUrl":"https://edge/a.jar",
                         "fileLength":10,"hashes":[]}]}"#,
        )
        .unwrap();
        assert_eq!(response.data[0].file_name, "a.jar");
        assert_eq!(response.data[0].file_length, 10);
    }
}
