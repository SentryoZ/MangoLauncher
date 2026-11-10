//! Artifact download planning and execution.
//!
//! Verification is deliberately separated from downloading: a plan can be
//! scanned against the local disk (repair mode) without touching the network,
//! and only offending files are queued.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;
use tracing::{debug, warn};

use crate::error::{MangoError, Result};
use crate::net::hash::{HashAlgorithm, hash_file};

/// What a planned file is for. Useful in progress reporting and lockfiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileKind {
    Client,
    Library,
    Asset,
    Native,
    Mod,
    Config,
    Loader,
}

/// A file the launcher must ensure exists with a specific content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlannedFile {
    pub kind: FileKind,
    /// Absolute destination path.
    pub path: PathBuf,
    pub url: String,
    pub size: u64,
    pub hash: Option<FileHash>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileHash {
    pub algorithm: HashAlgorithm,
    pub value: String,
}

impl FileHash {
    pub fn sha1(value: impl Into<String>) -> Self {
        Self { algorithm: HashAlgorithm::Sha1, value: value.into() }
    }

    pub fn sha256(value: impl Into<String>) -> Self {
        Self { algorithm: HashAlgorithm::Sha256, value: value.into() }
    }

    pub fn sha512(value: impl Into<String>) -> Self {
        Self { algorithm: HashAlgorithm::Sha512, value: value.into() }
    }
}

/// Pipeline progress, emitted for every verification and download step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    Verifying { done: usize, total: usize },
    Downloading { done: usize, total: usize, bytes_done: u64, bytes_total: u64, current: PathBuf },
    Finished { downloaded: usize, skipped: usize },
}

/// Result of comparing a plan against the local disk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanReport {
    pub present: usize,
    pub missing: Vec<PlannedFile>,
    pub corrupt: Vec<PlannedFile>,
}

impl ScanReport {
    pub fn invalid(&self) -> impl Iterator<Item = &PlannedFile> {
        self.missing.iter().chain(self.corrupt.iter())
    }

    pub fn is_clean(&self) -> bool {
        self.missing.is_empty() && self.corrupt.is_empty()
    }
}

/// Drop every file in `plan` that is already present and correct.
pub async fn scan(plan: &[PlannedFile], progress: &mut impl FnMut(Progress)) -> Result<ScanReport> {
    let mut report = ScanReport::default();

    for (index, file) in plan.iter().enumerate() {
        match &file.hash {
            None => {
                if file.path.exists() {
                    report.present += 1;
                } else {
                    report.missing.push(file.clone());
                }
            }
            Some(hash) => match hash_file(hash.algorithm, &file.path).await {
                Ok(actual) if actual.eq_ignore_ascii_case(&hash.value) => report.present += 1,
                Ok(_) => report.corrupt.push(file.clone()),
                Err(MangoError::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {
                    report.missing.push(file.clone());
                }
                Err(err) => return Err(err),
            },
        }

        progress(Progress::Verifying { done: index + 1, total: plan.len() });
    }

    Ok(report)
}

/// Download every invalid file, verifying each one before it is moved into place.
pub async fn download_all(
    client: &Client,
    files: &[PlannedFile],
    concurrency: usize,
    progress: &impl Fn(Progress),
) -> Result<usize> {
    let total = files.len();
    let bytes_total: u64 = files.iter().map(|file| file.size).sum();

    if total == 0 {
        progress(Progress::Finished { downloaded: 0, skipped: 0 });
        return Ok(0);
    }

    let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let byte_counter = Arc::new(std::sync::atomic::AtomicU64::new(0));

    let results = futures_util::stream::iter(files.iter().cloned())
        .map(|file| {
            let client = client.clone();
            let counter = Arc::clone(&counter);
            let byte_counter = Arc::clone(&byte_counter);
            async move {
                let outcome = download_one(&client, &file, &byte_counter).await;
                let done = counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
                progress(Progress::Downloading {
                    done,
                    total,
                    bytes_done: byte_counter.load(std::sync::atomic::Ordering::Relaxed),
                    bytes_total,
                    current: file.path.clone(),
                });
                outcome
            }
        })
        .buffer_unordered(concurrency.max(1))
        .collect::<Vec<_>>()
        .await;

    let mut downloaded = 0usize;
    for result in results {
        result?;
        downloaded += 1;
    }

    progress(Progress::Finished { downloaded, skipped: 0 });
    Ok(downloaded)
}

async fn download_one(
    client: &Client,
    file: &PlannedFile,
    byte_counter: &Arc<std::sync::atomic::AtomicU64>,
) -> Result<()> {
    if let Some(parent) = file.path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    // Write next to the target so a crash cannot leave a half-written file that
    // later looks valid; only a verified temp file is renamed into place.
    let temp = file.path.with_extension(format!(
        "{}.part",
        file.path.extension().and_then(|e| e.to_str()).unwrap_or("bin")
    ));

    let response = crate::net::http::get_with_retry(client, &file.url, None).await?;
    if let Some(expected) = response.content_length()
        && expected != file.size
        && file.size != 0
    {
        warn!(url = %file.url, expected, declared = file.size, "server size differs from plan");
    }

    let mut stream = response.bytes_stream();
    let mut output = tokio::fs::File::create(&temp).await?;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|err| MangoError::Http(err.to_string()))?;
        output.write_all(&chunk).await?;
        byte_counter.fetch_add(chunk.len() as u64, std::sync::atomic::Ordering::Relaxed);
    }
    output.flush().await?;
    drop(output);

    if let Some(hash) = &file.hash {
        let actual = hash_file(hash.algorithm, &temp).await?;
        if !actual.eq_ignore_ascii_case(&hash.value) {
            tokio::fs::remove_file(&temp).await.ok();
            return Err(MangoError::Other(format!(
                "hash mismatch for {}: expected {} {}, got {}",
                file.path.display(),
                hash.algorithm.name(),
                hash.value,
                actual
            )));
        }
    }

    tokio::fs::rename(&temp, &file.path).await?;
    debug!(path = %file.path.display(), "downloaded");
    Ok(())
}

/// Remove stale `.part` files left behind by an interrupted run.
pub async fn cleanup_partials(dir: &Path) -> Result<()> {
    let mut entries = tokio::fs::read_dir(dir).await?;
    while let Some(entry) = entries.next_entry().await? {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()).is_some_and(|ext| ext.ends_with(".part")) {
            tokio::fs::remove_file(&path).await.ok();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str, content: &[u8]) -> PlannedFile {
        PlannedFile {
            kind: FileKind::Asset,
            path: std::env::temp_dir().join(name),
            url: "https://example.invalid/x".to_owned(),
            size: content.len() as u64,
            hash: Some(FileHash {
                algorithm: HashAlgorithm::Sha256,
                value: crate::net::hash::hash_bytes(HashAlgorithm::Sha256, content),
            }),
        }
    }

    #[tokio::test]
    async fn scan_detects_missing_valid_and_corrupt() {
        let dir = std::env::temp_dir().join(format!("mango-scan-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();

        let good = file("scan-good.bin", b"good");
        tokio::fs::write(&good.path, b"good").await.unwrap();

        let bad_body = file("scan-bad.bin", b"expected");
        tokio::fs::write(&bad_body.path, b"tampered").await.unwrap();

        let missing = file("scan-missing.bin", b"nope");

        let plan = vec![good, bad_body.clone(), missing.clone()];
        let mut seen = Vec::new();
        let report = scan(&plan, &mut |p| seen.push(p)).await.unwrap();

        assert_eq!(report.present, 1);
        assert_eq!(report.corrupt.len(), 1);
        assert_eq!(report.corrupt[0].path, bad_body.path);
        assert_eq!(report.missing.len(), 1);
        assert_eq!(report.missing[0].path, missing.path);
        assert!(!report.is_clean());
        assert_eq!(seen.len(), 3);

        tokio::fs::remove_dir_all(&dir).await.ok();
    }

    #[tokio::test]
    async fn clean_plan_reports_no_work() {
        let path = std::env::temp_dir().join(format!("mango-clean-{}.bin", std::process::id()));
        tokio::fs::write(&path, b"abc").await.unwrap();

        let plan = vec![file(&format!("mango-clean-{}.bin", std::process::id()), b"abc")];
        let report = scan(&plan, &mut |_| {}).await.unwrap();
        assert!(report.is_clean());
        assert_eq!(report.present, 1);

        tokio::fs::remove_file(&path).await.ok();
    }
}
