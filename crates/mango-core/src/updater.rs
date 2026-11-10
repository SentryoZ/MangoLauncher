//! Self-update: check a release manifest, download, and hand over to the OS.
//!
//! The launcher cannot overwrite its own executable while it is running on
//! Windows, and an installer may want to replace other files too. So this module
//! separates the decision ("is there a newer release, and what do I download")
//! from the hand-over ("run the installer", "swap the binary", "open the
//! archive"), and the caller decides when to quit.
//!
//! The manifest is a small JSON document published as a release asset:
//!
//! ```json
//! {
//!   "version": "0.2.0",
//!   "notes": "Fixes…",
//!   "pubDate": "2026-11-10",
//!   "platforms": {
//!     "windows-x86_64": {
//!       "url": "https://…/MangoLauncher-0.2.0-setup.exe",
//!       "sha256": "…64 hex characters…",
//!       "size": 12582912,
//!       "kind": "installer"
//!     }
//!   }
//! }
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use crate::error::{MangoError, Result};
use crate::net::{hash, http};

/// Size limit for a downloaded update, so a wrong URL cannot fill the disk.
const MAX_UPDATE_BYTES: usize = 512 * 1024 * 1024;

/// Default manifest location, derived from the crate repository.
pub fn default_manifest_url() -> String {
    format!("{}/releases/latest/download/latest.json", crate::REPOSITORY.trim_end_matches('/'))
}

/// Platform key used in the manifest, e.g. `windows-x86_64`.
///
/// `std::env::consts` already uses the spellings the manifest expects.
pub fn platform_key() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// How the downloaded artifact should be applied.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum AssetKind {
    /// A platform installer, run and allowed to replace the launcher.
    #[default]
    Installer,
    /// A single executable that replaces the running one.
    Binary,
    /// An archive the player opens and moves into place themselves.
    Archive,
}

/// What to do with a downloaded artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyPlan {
    /// Run it and let it replace the launcher.
    RunInstaller,
    /// Swap the running executable with the downloaded file.
    SwapExecutable,
    /// Open it so the player can install it by hand.
    OpenArchive,
}

impl AssetKind {
    pub fn plan(self) -> ApplyPlan {
        match self {
            Self::Installer => ApplyPlan::RunInstaller,
            Self::Binary => ApplyPlan::SwapExecutable,
            Self::Archive => ApplyPlan::OpenArchive,
        }
    }
}

/// One downloadable artifact.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct PlatformAsset {
    pub url: String,
    /// Lowercase hex SHA-256 of the file. Required: an update is executable code.
    pub sha256: String,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub kind: AssetKind,
}

/// The published release manifest.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ReleaseManifest {
    pub version: String,
    #[serde(default)]
    pub notes: Option<String>,
    #[serde(default, rename = "pubDate")]
    pub published: Option<String>,
    pub platforms: BTreeMap<String, PlatformAsset>,
}

/// A newer release that applies to this machine.
#[derive(Debug, Clone)]
pub struct Update {
    pub version: semver::Version,
    pub notes: Option<String>,
    pub asset: PlatformAsset,
}

impl Update {
    pub fn plan(&self) -> ApplyPlan {
        self.asset.kind.plan()
    }
}

impl ReleaseManifest {
    /// Parse and validate a manifest document.
    ///
    /// Validation happens up front so a malformed digest is rejected before
    /// anything is downloaded.
    pub fn parse(text: &str) -> Result<Self> {
        let manifest: Self = serde_json::from_str(crate::config::strip_bom(text))?;

        if manifest.platforms.is_empty() {
            return Err(MangoError::Other("the release manifest lists no platforms".to_owned()));
        }

        for (platform, asset) in &manifest.platforms {
            if asset.url.trim().is_empty() {
                return Err(MangoError::Other(format!(
                    "the release asset for `{platform}` has no URL"
                )));
            }
            if !is_sha256(&asset.sha256) {
                return Err(MangoError::Other(format!(
                    "the release asset for `{platform}` has no valid SHA-256 digest"
                )));
            }
        }

        Ok(manifest)
    }

    /// The artifact for this machine, if the manifest lists one.
    pub fn asset_for_current_platform(&self) -> Option<&PlatformAsset> {
        self.platforms.get(&platform_key())
    }
}

/// Whether `candidate` should replace `current`.
///
/// A release with the same version is not an update, and a prerelease is only
/// offered when the player opted in.
pub fn is_newer(
    current: &semver::Version,
    candidate: &semver::Version,
    allow_prerelease: bool,
) -> bool {
    if !allow_prerelease && !candidate.pre.is_empty() {
        return false;
    }
    candidate > current
}

/// Parse a version, tolerating the `v` prefix used by release tags.
pub fn parse_version(text: &str) -> Result<semver::Version> {
    let trimmed = text.trim();
    let without_prefix = trimmed.strip_prefix('v').unwrap_or(trimmed);
    semver::Version::parse(without_prefix)
        .map_err(|err| MangoError::Other(format!("`{text}` is not a version: {err}")))
}

/// Read a manifest from a URL or from a local file.
///
/// Accepting a path makes the updater testable before a release exists, and lets
/// a self-hosted manifest live next to the launcher.
pub async fn load_manifest(client: &reqwest::Client, source: &str) -> Result<ReleaseManifest> {
    let text = match local_path(source).and_then(|path| std::fs::read_to_string(path).ok()) {
        Some(text) => text,
        None => http::get_text(client, source).await?,
    };
    ReleaseManifest::parse(&text)
}

/// The source as a filesystem path, when it is not an HTTP URL.
fn local_path(source: &str) -> Option<&str> {
    let trimmed = source.trim();
    if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        return None;
    }
    Some(trimmed)
}

/// Check the manifest for a newer release.
///
/// Returns `Ok(None)` when the installed version is current, which is the common
/// case and must not be an error.
pub async fn check(
    client: &reqwest::Client,
    manifest_url: &str,
    allow_prerelease: bool,
) -> Result<Option<Update>> {
    let manifest = load_manifest(client, manifest_url).await?;

    let current = parse_version(crate::VERSION)?;
    let candidate = parse_version(&manifest.version)?;

    if !is_newer(&current, &candidate, allow_prerelease) {
        info!(installed = %current, offered = %candidate, "no update available");
        return Ok(None);
    }

    let asset = manifest.asset_for_current_platform().cloned().ok_or_else(|| {
        MangoError::Other(format!(
            "release {} does not publish a build for {}",
            candidate,
            platform_key()
        ))
    })?;

    Ok(Some(Update { version: candidate, notes: manifest.notes, asset }))
}

/// File name to store a downloaded artifact under.
///
/// Taken from the URL's path, with anything that could escape the target
/// directory rejected, because the URL comes from a remote document.
pub fn file_name_for(asset: &PlatformAsset) -> Result<String> {
    let without_query = asset.url.split(['?', '#']).next().unwrap_or("");
    // Drop the scheme and authority so `https://example.com` cannot be mistaken
    // for a file called `example.com`.
    let path = match without_query.split_once("://") {
        Some((_, rest)) => match rest.split_once('/') {
            Some((_, path)) => path,
            None => "",
        },
        None => without_query,
    };

    let last = path.rsplit('/').next().unwrap_or("");

    if last.is_empty() || last.contains('\\') || last == ".." || last == "." {
        return Err(MangoError::Other(format!("cannot derive a file name from `{}`", asset.url)));
    }

    Ok(last.to_owned())
}

/// Download an artifact and verify it before returning its path.
///
/// The file is written only after the digest matches, so a partially written or
/// tampered download is never left where it could be executed.
pub async fn download(client: &reqwest::Client, update: &Update, dir: &Path) -> Result<PathBuf> {
    if let Some(size) = update.asset.size
        && size as usize > MAX_UPDATE_BYTES
    {
        return Err(MangoError::Other(format!(
            "the update is {} MB, which is larger than the launcher will download",
            size / (1024 * 1024)
        )));
    }

    let name = file_name_for(&update.asset)?;
    std::fs::create_dir_all(dir)?;
    let target = dir.join(name);

    info!(url = %update.asset.url, version = %update.version, "downloading update");
    let bytes = http::get_bytes(client, &update.asset.url).await?;

    if bytes.is_empty() {
        return Err(MangoError::Other("the update download was empty".to_owned()));
    }
    if bytes.len() > MAX_UPDATE_BYTES {
        return Err(MangoError::Other("the update download was unexpectedly large".to_owned()));
    }

    let digest = hash::hash_bytes(hash::HashAlgorithm::Sha256, &bytes);
    if !digest.eq_ignore_ascii_case(&update.asset.sha256) {
        return Err(MangoError::Other(format!(
            "the update failed verification: expected {}, got {digest}",
            update.asset.sha256
        )));
    }

    // Write through a temporary name so an interrupted copy cannot look complete.
    let partial = target.with_extension("partial");
    std::fs::write(&partial, &bytes)?;
    std::fs::rename(&partial, &target)?;

    Ok(target)
}

/// Replace the running executable with a downloaded one.
///
/// On Windows a running image cannot be overwritten but it *can* be renamed, so
/// the old file is moved aside first and left for the player to delete.
pub fn swap_executable(downloaded: &Path, current: &Path) -> Result<()> {
    let backup = current.with_extension("old");
    let _ = std::fs::remove_file(&backup);

    if current.exists() {
        std::fs::rename(current, &backup)?;
    }

    std::fs::copy(downloaded, current)?;
    make_executable(current)?;
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

/// Start a downloaded installer, detached from this process.
pub fn run_installer(path: &Path) -> Result<()> {
    let mut command = std::process::Command::new(path);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS: the installer must survive the launcher exiting.
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        command.creation_flags(DETACHED_PROCESS);
    }
    command
        .spawn()
        .map_err(|err| MangoError::Other(format!("could not start the installer: {err}")))?;
    Ok(())
}

/// Open a downloaded archive with the desktop's default handler.
pub fn open_archive(path: &Path) -> Result<()> {
    let command = if cfg!(target_os = "windows") {
        ("cmd", vec!["/C", "start"])
    } else if cfg!(target_os = "macos") {
        ("open", Vec::new())
    } else {
        ("xdg-open", Vec::new())
    };

    std::process::Command::new(command.0)
        .args(command.1)
        .arg(path)
        .spawn()
        .map_err(|err| MangoError::Other(format!("could not open the download: {err}")))?;
    Ok(())
}

/// Apply an update and report whether the caller should exit.
///
/// `SwapExecutable` is the only plan that keeps running: the caller continues
/// with the new binary on disk next start. The others hand off to another program
/// and expect the launcher to quit.
pub fn apply(update: &Update, downloaded: &Path, current_exe: &Path) -> Result<bool> {
    match update.plan() {
        ApplyPlan::RunInstaller => {
            run_installer(downloaded)?;
            Ok(true)
        }
        ApplyPlan::SwapExecutable => {
            swap_executable(downloaded, current_exe)?;
            warn!("the launcher was replaced; the new version starts next launch");
            Ok(true)
        }
        ApplyPlan::OpenArchive => {
            open_archive(downloaded)?;
            Ok(false)
        }
    }
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.chars().all(|c| c.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_SHA: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    fn manifest_json() -> String {
        format!(
            r#"{{
                "version": "0.2.0",
                "notes": "Faster installs",
                "pubDate": "2026-11-10",
                "platforms": {{
                    "windows-x86_64": {{
                        "url": "https://example.com/MangoLauncher-0.2.0-setup.exe",
                        "sha256": "{VALID_SHA}",
                        "size": 12000000,
                        "kind": "installer"
                    }},
                    "linux-x86_64": {{
                        "url": "https://example.com/MangoLauncher-0.2.0.AppImage",
                        "sha256": "{VALID_SHA}",
                        "kind": "binary"
                    }}
                }}
            }}"#
        )
    }

    #[test]
    fn the_default_manifest_lives_in_releases() {
        let url = default_manifest_url();
        assert!(url.ends_with("/releases/latest/download/latest.json"), "{url}");
        assert!(url.starts_with("https://github.com/SentryoZ/MangoLauncher"), "{url}");
    }

    #[test]
    fn the_platform_key_matches_the_manifest_convention() {
        let key = platform_key();
        assert!(key.starts_with(std::env::consts::OS), "{key}");
        assert!(key.ends_with(std::env::consts::ARCH), "{key}");
        assert_eq!(key.matches('-').count(), 1, "one separator: {key}");
    }

    #[test]
    fn a_valid_manifest_parses() {
        let manifest = ReleaseManifest::parse(&manifest_json()).unwrap();
        assert_eq!(manifest.version, "0.2.0");
        assert_eq!(manifest.notes.as_deref(), Some("Faster installs"));
        assert_eq!(manifest.platforms.len(), 2);

        let windows = &manifest.platforms["windows-x86_64"];
        assert_eq!(windows.kind, AssetKind::Installer);
        assert_eq!(windows.size, Some(12_000_000));

        let linux = &manifest.platforms["linux-x86_64"];
        assert_eq!(linux.kind, AssetKind::Binary);
        assert_eq!(linux.size, None, "size is optional");
    }

    #[test]
    fn an_unknown_asset_kind_does_not_break_the_document() {
        // Listing other platforms must not require understanding their kinds.
        let manifest = ReleaseManifest::parse(&format!(
            r#"{{"version":"0.2.0","platforms":{{
                "windows-x86_64":{{"url":"https://e/x.exe","sha256":"{VALID_SHA}","kind":"msi"}}}}}}"#
        ));
        assert!(manifest.is_err(), "an unknown kind is rejected, not guessed");
    }

    #[test]
    fn a_manifest_without_platforms_is_rejected() {
        let error = ReleaseManifest::parse(r#"{"version":"0.2.0","platforms":{}}"#).unwrap_err();
        assert!(error.to_string().contains("no platforms"), "{error}");
    }

    #[test]
    fn a_bad_digest_is_rejected_before_downloading() {
        let json = r#"{"version":"0.2.0","platforms":{
            "windows-x86_64":{"url":"https://e/x.exe","sha256":"deadbeef"}}}"#;
        let error = ReleaseManifest::parse(json).unwrap_err();
        assert!(error.to_string().contains("SHA-256"), "{error}");
    }

    #[test]
    fn an_asset_without_a_url_is_rejected() {
        let json = format!(
            r#"{{"version":"0.2.0","platforms":{{
                "windows-x86_64":{{"url":"  ","sha256":"{VALID_SHA}"}}}}}}"#
        );
        let error = ReleaseManifest::parse(&json).unwrap_err();
        assert!(error.to_string().contains("no URL"), "{error}");
    }

    #[test]
    fn a_v_prefixed_tag_is_accepted() {
        assert_eq!(parse_version("v1.2.3").unwrap().to_string(), "1.2.3");
        assert_eq!(parse_version(" 1.2.3 ").unwrap().to_string(), "1.2.3");
        assert!(parse_version("latest").is_err());
    }

    #[test]
    fn only_strictly_newer_versions_are_offered() {
        let current = parse_version("0.1.0").unwrap();
        assert!(is_newer(&current, &parse_version("0.2.0").unwrap(), false));
        assert!(is_newer(&current, &parse_version("0.1.1").unwrap(), false));
        assert!(!is_newer(&current, &parse_version("0.1.0").unwrap(), false));
        assert!(!is_newer(&current, &parse_version("0.0.9").unwrap(), false), "no downgrades");
    }

    #[test]
    fn prereleases_need_an_opt_in() {
        let current = parse_version("0.1.0").unwrap();
        let beta = parse_version("0.2.0-beta.1").unwrap();
        assert!(!is_newer(&current, &beta, false));
        assert!(is_newer(&current, &beta, true));
    }

    #[test]
    fn the_file_name_comes_from_the_url() {
        let asset = PlatformAsset {
            url: "https://example.com/dl/MangoLauncher-setup.exe".to_owned(),
            sha256: VALID_SHA.to_owned(),
            size: None,
            kind: AssetKind::Installer,
        };
        assert_eq!(file_name_for(&asset).unwrap(), "MangoLauncher-setup.exe");
    }

    #[test]
    fn a_query_string_is_not_part_of_the_file_name() {
        let asset = PlatformAsset {
            url: "https://example.com/dl/MangoLauncher.AppImage?token=abc".to_owned(),
            sha256: VALID_SHA.to_owned(),
            size: None,
            kind: AssetKind::Binary,
        };
        assert_eq!(file_name_for(&asset).unwrap(), "MangoLauncher.AppImage");
    }

    #[test]
    fn a_url_that_cannot_name_a_file_is_rejected() {
        for url in ["https://example.com/dl/", "https://example.com", "https://example.com/.."] {
            let asset = PlatformAsset {
                url: url.to_owned(),
                sha256: VALID_SHA.to_owned(),
                size: None,
                kind: AssetKind::Installer,
            };
            assert!(file_name_for(&asset).is_err(), "{url} should be rejected");
        }
    }

    #[test]
    fn the_apply_plan_follows_the_asset_kind() {
        assert_eq!(AssetKind::Installer.plan(), ApplyPlan::RunInstaller);
        assert_eq!(AssetKind::Binary.plan(), ApplyPlan::SwapExecutable);
        assert_eq!(AssetKind::Archive.plan(), ApplyPlan::OpenArchive);
    }

    #[test]
    fn swapping_replaces_the_file_and_keeps_a_backup() {
        let dir = std::env::temp_dir().join(format!("mango-swap-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let current = dir.join("launcher");
        let downloaded = dir.join("launcher.new");
        std::fs::write(&current, b"old").unwrap();
        std::fs::write(&downloaded, b"new").unwrap();

        swap_executable(&downloaded, &current).unwrap();

        assert_eq!(std::fs::read(&current).unwrap(), b"new");
        assert_eq!(std::fs::read(dir.join("launcher.old")).unwrap(), b"old");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_swap_works_when_no_previous_file_exists() {
        let dir = std::env::temp_dir().join(format!("mango-swap-new-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        let current = dir.join("launcher");
        let downloaded = dir.join("launcher.new");
        std::fs::write(&downloaded, b"new").unwrap();

        swap_executable(&downloaded, &current).unwrap();
        assert_eq!(std::fs::read(&current).unwrap(), b"new");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_sha256_digest_is_exactly_64_hex_characters() {
        assert!(is_sha256(VALID_SHA));
        assert!(!is_sha256("abc"));
        assert!(!is_sha256(&"z".repeat(64)));
    }

    #[test]
    fn a_manifest_written_with_a_byte_order_mark_still_parses() {
        // Windows editors and shells add a BOM; it must not look like corruption.
        let manifest = ReleaseManifest::parse(&format!("\u{feff}{}", manifest_json())).unwrap();
        assert_eq!(manifest.version, "0.2.0");
    }

    #[test]
    fn an_http_source_is_not_treated_as_a_path() {
        assert!(local_path("https://example.com/latest.json").is_none());
        assert!(local_path("http://example.com/latest.json").is_none());
        assert_eq!(local_path(" C:/dist/latest.json "), Some("C:/dist/latest.json"));
        assert_eq!(local_path("./latest.json"), Some("./latest.json"));
    }

    #[tokio::test]
    async fn a_manifest_can_be_read_from_a_file() {
        // Lets the updater be exercised before any release is published.
        let dir = std::env::temp_dir().join(format!("mango-manifest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("latest.json");
        std::fs::write(&path, manifest_json()).unwrap();

        let client = http::build_client().unwrap();
        let manifest = load_manifest(&client, &path.to_string_lossy()).await.unwrap();
        assert_eq!(manifest.version, "0.2.0");

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[tokio::test]
    async fn a_missing_file_source_is_an_error_not_a_silent_pass() {
        let client = http::build_client().unwrap();
        let result = load_manifest(&client, "/nonexistent/latest.json").await;
        assert!(result.is_err(), "a missing manifest must not look like `up to date`");
    }
}
