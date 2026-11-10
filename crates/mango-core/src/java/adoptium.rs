//! Downloading a JDK from Adoptium when no suitable runtime is installed.

use std::path::{Path, PathBuf};

use reqwest::Client;
use serde::Deserialize;
use tracing::info;

use crate::error::{MangoError, Result};
use crate::net::http;

const ADOPTIUM_ASSETS: &str = "https://api.adoptium.net/v3/assets/latest";

#[derive(Debug, Deserialize)]
struct AssetResponse {
    binary: Binary,
    version: VersionInfo,
    release_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Binary {
    package: Package,
}

#[derive(Debug, Deserialize)]
struct Package {
    link: String,
    name: String,
    size: u64,
    checksum: String,
}

#[derive(Debug, Deserialize)]
struct VersionInfo {
    semver: String,
}

/// A resolved JDK download.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JdkDownload {
    pub url: String,
    pub file_name: String,
    pub size: u64,
    pub sha256: String,
    pub version: String,
    pub release_name: Option<String>,
}

/// Map the running platform to Adoptium's naming.
pub fn platform_slug() -> (&'static str, &'static str) {
    let os = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "linux"
    };

    let arch = match std::env::consts::ARCH {
        "x86" => "x86",
        "aarch64" => "aarch64",
        other => {
            let _ = other;
            "x64"
        }
    };

    (os, arch)
}

/// Ask Adoptium for the newest JDK of a given major version.
pub async fn resolve(client: &Client, major: u32) -> Result<JdkDownload> {
    let (os, arch) = platform_slug();
    let url = format!(
        "{ADOPTIUM_ASSETS}/{major}/hotspot?os={os}&architecture={arch}&image_type=jdk&vendor=eclipse"
    );

    let assets: Vec<AssetResponse> = http::get_json(client, &url).await?;
    let asset = assets
        .into_iter()
        .next()
        .ok_or_else(|| MangoError::Other(format!("Adoptium has no JDK {major} for {os}/{arch}")))?;

    Ok(JdkDownload {
        url: asset.binary.package.link,
        file_name: asset.binary.package.name,
        size: asset.binary.package.size,
        sha256: asset.binary.package.checksum,
        version: asset.version.semver,
        release_name: asset.release_name,
    })
}

/// Download and extract a JDK into `<java_dir>/<vendor>-<major>`, returning the
/// `java` executable path.
pub async fn install(
    client: &Client,
    java_dir: &Path,
    download: &JdkDownload,
    major: u32,
) -> Result<PathBuf> {
    std::fs::create_dir_all(java_dir)?;

    let target = java_dir.join(format!("temurin-{major}"));
    let executable = super::discover::java_binary_name();

    // Already extracted from a previous run.
    if let Some(existing) = find_executable(&target, executable) {
        info!(path = %existing.display(), "jdk already extracted");
        return Ok(existing);
    }

    let archive_path = java_dir.join(&download.file_name);
    info!(url = %download.url, target = %archive_path.display(), "downloading jdk");

    let bytes = http::get_bytes(client, &download.url).await?;
    if bytes.len() as u64 != download.size {
        return Err(MangoError::Other(format!(
            "jdk download size mismatch: expected {}, got {}",
            download.size,
            bytes.len()
        )));
    }
    let actual = crate::net::hash::hash_bytes(crate::net::hash::HashAlgorithm::Sha256, &bytes);
    if !actual.eq_ignore_ascii_case(&download.sha256) {
        return Err(MangoError::Other(format!(
            "jdk checksum mismatch: expected {}, got {actual}",
            download.sha256
        )));
    }
    tokio::fs::write(&archive_path, &bytes).await?;

    // The archive wraps everything in one top-level directory; strip it so the
    // layout is predictable regardless of the vendor's directory name.
    let staging = java_dir.join(format!(".extract-{major}"));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)?;
    }
    std::fs::create_dir_all(&staging)?;

    if download.file_name.ends_with(".zip") {
        extract_zip(&archive_path, &staging)?;
    } else {
        return Err(MangoError::Other(format!(
            "unsupported jdk archive format: {}",
            download.file_name
        )));
    }

    let root = single_subdirectory(&staging).unwrap_or_else(|| staging.clone());
    if target.exists() {
        std::fs::remove_dir_all(&target)?;
    }
    std::fs::rename(&root, &target)?;
    std::fs::remove_dir_all(&staging).ok();
    std::fs::remove_file(&archive_path).ok();

    find_executable(&target, executable).ok_or_else(|| {
        MangoError::Other(format!("no {executable} inside extracted jdk at {}", target.display()))
    })
}

fn extract_zip(archive: &Path, target: &Path) -> Result<()> {
    let file = std::fs::File::open(archive)?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|err| MangoError::Other(format!("{} is not a zip: {err}", archive.display())))?;

    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|err| MangoError::Other(format!("bad zip entry: {err}")))?;

        let Some(relative) = entry.enclosed_name() else { continue };
        let destination = target.join(relative);

        if entry.is_dir() {
            std::fs::create_dir_all(&destination)?;
            continue;
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut output = std::fs::File::create(&destination)?;
        std::io::copy(&mut entry, &mut output)?;

        // Restore the executable bit for `bin/java` and friends on unix.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Some(mode) = entry.unix_mode() {
                let _ =
                    std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(mode));
            }
        }
    }
    Ok(())
}

fn single_subdirectory(dir: &Path) -> Option<PathBuf> {
    let mut entries = std::fs::read_dir(dir).ok()?.flatten();
    let first = entries.next()?;
    if entries.next().is_some() {
        return None;
    }
    first.file_type().ok().filter(|t| t.is_dir()).map(|_| first.path())
}

/// Search a JDK tree for its `bin/java` executable.
fn find_executable(root: &Path, executable: &str) -> Option<PathBuf> {
    let direct = root.join("bin").join(executable);
    if direct.is_file() {
        return Some(direct);
    }
    // macOS bundles keep the home directory one level deeper.
    let macos = root.join("Contents").join("Home").join("bin").join(executable);
    if macos.is_file() {
        return Some(macos);
    }

    for entry in std::fs::read_dir(root).ok()?.flatten() {
        if entry.file_type().ok()?.is_dir()
            && let Some(found) = find_executable(&entry.path(), executable)
        {
            return Some(found);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_slug_is_known() {
        let (os, arch) = platform_slug();
        assert!(["windows", "mac", "linux"].contains(&os));
        assert!(["x86", "x64", "aarch64"].contains(&arch));
    }

    #[test]
    fn nested_executable_is_found() {
        let root = std::env::temp_dir().join(format!("mango-jdk-{}", std::process::id()));
        let bin = root.join("jdk-17").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let exe = bin.join(super::super::discover::java_binary_name());
        std::fs::write(&exe, b"").unwrap();

        let found = find_executable(&root, super::super::discover::java_binary_name());
        assert_eq!(found, Some(exe));

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn single_subdirectory_is_detected() {
        let root = std::env::temp_dir().join(format!("mango-jdk-single-{}", std::process::id()));
        std::fs::create_dir_all(root.join("jdk-17.0.9")).unwrap();
        assert_eq!(single_subdirectory(&root), Some(root.join("jdk-17.0.9")));

        std::fs::create_dir_all(root.join("extra")).unwrap();
        assert_eq!(single_subdirectory(&root), None);

        std::fs::remove_dir_all(&root).ok();
    }
}
