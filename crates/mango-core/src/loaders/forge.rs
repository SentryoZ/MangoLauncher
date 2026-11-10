//! Forge and NeoForge installation.
//!
//! Unlike Fabric, there is no published `version.json` to download: the
//! installer patches the client and *generates* the profile. Parsing their
//! processor chain ourselves would mean reimplementing decompilation and
//! binary patching, so the official installer is run headlessly:
//!
//! ```text
//! java -jar forge-installer.jar --installClient <game-dir>
//! ```
//!
//! After a successful run the generated `versions/<id>/<id>.json` is read and
//! merged like any other profile.

use std::path::Path;
use std::process::Stdio;

use serde::Deserialize;
use tracing::{info, warn};

use crate::error::{MangoError, Result};
use crate::loaders::LoaderProfile;
use crate::mojang::VersionJson;
use crate::net::http;

pub const FORGE_MAVEN: &str = "https://maven.minecraftforge.net";
pub const FORGE_PROMOTIONS: &str =
    "https://files.minecraftforge.net/net/minecraftforge/forge/promotions_slim.json";
/// Maps each game version to every published Forge build.
pub const FORGE_MAVEN_METADATA: &str =
    "https://files.minecraftforge.net/net/minecraftforge/forge/maven-metadata.json";
pub const NEOFORGE_MAVEN: &str = "https://maven.neoforged.net/releases";
pub const NEOFORGE_MAVEN_METADATA: &str =
    "https://maven.neoforged.net/releases/net/neoforged/neoforge/maven-metadata.xml";

#[derive(Debug, Deserialize)]
struct Promotions {
    #[serde(default)]
    promos: std::collections::BTreeMap<String, String>,
}

/// Newest Forge build for a game version.
///
/// Prefers the `recommended` promotion and falls back to `latest`, which is what
/// the Forge website recommends to players.
pub async fn latest_forge_version(client: &reqwest::Client, minecraft: &str) -> Result<String> {
    let promotions: Promotions = http::get_json(client, FORGE_PROMOTIONS).await?;

    for channel in ["recommended", "latest"] {
        let key = format!("{minecraft}-{channel}");
        if let Some(version) = promotions.promos.get(&key) {
            return Ok(version.clone());
        }
    }

    Err(MangoError::Other(format!("Forge has no build for Minecraft {minecraft}")))
}

/// Full Forge version string, e.g. `1.20.1-47.2.0`.
pub fn forge_full_version(minecraft: &str, forge_version: &str) -> String {
    if forge_version.starts_with(minecraft) {
        forge_version.to_owned()
    } else {
        format!("{minecraft}-{forge_version}")
    }
}

/// URL of the Forge installer jar.
pub fn forge_installer_url(full_version: &str) -> String {
    format!(
        "{FORGE_MAVEN}/net/minecraftforge/forge/{full_version}/forge-{full_version}-installer.jar"
    )
}

/// Map a Minecraft version to its NeoForge version prefix.
///
/// NeoForge dropped the `1.` prefix from 1.20.2 onward, so `1.20.1` maps to
/// `47.1.x` while `1.21.1` maps to `21.1.x`.
pub fn neoforge_prefix(minecraft: &str) -> Option<String> {
    let mut pieces = minecraft.split('.');
    let major: u32 = pieces.next()?.parse().ok()?;
    if major != 1 {
        return None;
    }
    let minor: u32 = pieces.next()?.parse().ok()?;
    let patch: u32 = pieces.next().and_then(|p| p.parse().ok()).unwrap_or(0);

    // 1.20.1 is the last release that shipped a Forge-based `1.20.1-47.x` line.
    if (minor, patch) == (20, 1) {
        return Some("47.1".to_owned());
    }
    Some(format!("{minor}.{patch}"))
}

/// Pick the newest NeoForge version matching the game version.
pub async fn latest_neoforge_version(client: &reqwest::Client, minecraft: &str) -> Result<String> {
    let prefix = neoforge_prefix(minecraft).ok_or_else(|| {
        MangoError::Other(format!("NeoForge does not support Minecraft {minecraft}"))
    })?;

    let url = format!("{NEOFORGE_MAVEN}/net/neoforged/neoforge/maven-metadata.xml");
    let text = http::get_text(client, &url).await?;

    let mut best: Option<semver::Version> = None;
    for version in parse_maven_versions(&text) {
        if !version.starts_with(&format!("{prefix}.")) {
            continue;
        }
        // NeoForge versions can carry a `-beta` suffix, which semver handles.
        if let Ok(parsed) = semver::Version::parse(&version)
            && best.as_ref().is_none_or(|current| parsed > *current)
        {
            best = Some(parsed);
        }
    }

    best.map(|version| version.to_string()).ok_or_else(|| {
        MangoError::Other(format!("NeoForge has no build for Minecraft {minecraft}"))
    })
}

/// URL of the NeoForge installer jar.
pub fn neoforge_installer_url(version: &str) -> String {
    format!("{NEOFORGE_MAVEN}/net/neoforged/neoforge/{version}/neoforge-{version}-installer.jar")
}

/// Extract `<version>` values from a maven `maven-metadata.xml`.
pub fn parse_maven_versions(xml: &str) -> Vec<String> {
    let mut versions = Vec::new();
    let mut rest = xml;

    while let Some(start) = rest.find("<version>") {
        let after = &rest[start + "<version>".len()..];
        let Some(end) = after.find("</version>") else { break };
        versions.push(after[..end].trim().to_owned());
        rest = &after[end + "</version>".len()..];
    }
    versions
}

/// Every Forge build for a game version, newest first.
///
/// The maven metadata maps a game version to full loader ids, so the game part is
/// stripped to leave the bare loader version that manifests and the picker use.
/// Old releases order it the other way round — `1.20.1-47.4.29` for modern builds
/// but `10.13.4.1614-1.7.10` for 1.7.10 — so both forms are handled.
pub async fn version_list(client: &reqwest::Client, minecraft: &str) -> Result<Vec<String>> {
    let index: std::collections::BTreeMap<String, Vec<String>> =
        http::get_json(client, FORGE_MAVEN_METADATA).await?;

    let Some(entries) = index.get(minecraft) else { return Ok(Vec::new()) };

    let mut versions: Vec<String> =
        entries.iter().map(|full| strip_game_version(full, minecraft)).collect();

    // Maven lists oldest-first; players expect the newest at the top.
    versions.reverse();
    Ok(versions)
}

/// Remove the game version from a Forge maven id.
///
/// The position varies by era and sometimes appears twice: modern builds are
/// `1.20.1-47.4.29`, 1.7.10-era builds are `1.7.10-10.13.4.1614-1.7.10`, and a
/// bare `47.4.29` must be left untouched.
fn strip_game_version(full: &str, minecraft: &str) -> String {
    let prefix = format!("{minecraft}-");
    let suffix = format!("-{minecraft}");

    let mut rest = full.strip_prefix(&prefix).unwrap_or(full);
    if let Some(stripped) = rest.strip_suffix(&suffix) {
        rest = stripped;
    }
    rest.to_owned()
}

/// Every NeoForge build for a game version, newest first.
pub async fn neoforge_version_list(
    client: &reqwest::Client,
    minecraft: &str,
) -> Result<Vec<String>> {
    let prefix = neoforge_prefix(minecraft).ok_or_else(|| {
        MangoError::Other(format!("NeoForge does not support Minecraft {minecraft}"))
    })?;

    let text = http::get_text(client, NEOFORGE_MAVEN_METADATA).await?;
    let wanted = format!("{prefix}.");

    let mut versions: Vec<(semver::Version, String)> = parse_maven_versions(&text)
        .into_iter()
        .filter(|version| version.starts_with(&wanted))
        .filter_map(|version| semver::Version::parse(&version).ok().map(|parsed| (parsed, version)))
        .collect();

    versions.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(versions.into_iter().map(|(_, text)| text).collect())
}

/// Download and run the Forge installer, then load the generated profile.
pub async fn resolve_forge(
    client: &reqwest::Client,
    common_dir: &Path,
    minecraft: &str,
    version: Option<&str>,
) -> Result<LoaderProfile> {
    let forge_version = match version {
        Some(version) => version.to_owned(),
        None => latest_forge_version(client, minecraft).await?,
    };
    let full_version = forge_full_version(minecraft, &forge_version);
    // Forge writes its profile under `versions/<mc>-forge-<version>/`, which is
    // not the same as the maven version string.
    install_with_installer(
        client,
        common_dir,
        &forge_installer_url(&full_version),
        &format!("{minecraft}-forge-{forge_version}"),
        minecraft,
    )
    .await
}

/// Download and run the NeoForge installer, then load the generated profile.
pub async fn resolve_neoforge(
    client: &reqwest::Client,
    common_dir: &Path,
    minecraft: &str,
    version: Option<&str>,
) -> Result<LoaderProfile> {
    let neoforge_version = match version {
        Some(version) => version.to_owned(),
        None => latest_neoforge_version(client, minecraft).await?,
    };

    // The generated profile id is `neoforge-<version>`.
    install_with_installer(
        client,
        common_dir,
        &neoforge_installer_url(&neoforge_version),
        &format!("neoforge-{neoforge_version}"),
        minecraft,
    )
    .await
}

/// Shared installer runner.
///
/// `profile_id` is the directory name the installer writes under `versions/`,
/// which for Forge is the full version and for NeoForge is `neoforge-<version>`.
async fn install_with_installer(
    client: &reqwest::Client,
    common_dir: &Path,
    url: &str,
    profile_id: &str,
    minecraft: &str,
) -> Result<LoaderProfile> {
    info!(%url, profile_id, "downloading loader installer");
    let bytes = http::get_bytes(client, url).await?;

    let temp_dir = std::env::temp_dir().join(format!("mango-installer-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp_dir)?;
    let installer = temp_dir.join("installer.jar");
    std::fs::write(&installer, &bytes)?;

    // The installer needs a JVM. The version must match what the game needs
    // (17+ for 1.17+), so the caller may override discovery via MANGO_JAVA.
    let java = std::env::var("MANGO_JAVA")
        .unwrap_or_else(|_| crate::java::discover::java_binary_name().to_owned());

    std::fs::create_dir_all(common_dir)?;

    // The Forge/NeoForge installer refuses to run unless the target looks like a
    // launcher installation. It only checks that these files exist, so write a
    // minimal profile rather than requiring the player to own the vanilla
    // launcher.
    ensure_launcher_profiles(common_dir)?;

    info!(java = %java, target = %common_dir.display(), "running loader installer");
    let output = tokio::process::Command::new(&java)
        .arg("-jar")
        .arg(&installer)
        .arg("--installClient")
        .arg(common_dir)
        .current_dir(&temp_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|err| {
            MangoError::Other(format!("could not run the installer with {java}: {err}"))
        })?;

    let log = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::remove_dir_all(&temp_dir).ok();

    if !output.status.success() {
        // The installer's own log lives next to it and is already deleted; the
        // captured output is the only diagnostic left, so surface its tail.
        let tail: String = log.lines().rev().take(15).collect::<Vec<_>>().join("\n");
        return Err(MangoError::Other(format!(
            "loader installer exited with {:?}\n{tail}",
            output.status.code()
        )));
    }

    let profile_path =
        common_dir.join("versions").join(profile_id).join(format!("{profile_id}.json"));

    if !profile_path.exists() {
        warn!(path = %profile_path.display(), "installer reported success but no profile was written");
        return Err(MangoError::Other(format!(
            "installer did not create {}; check the installer log",
            profile_path.display()
        )));
    }

    let text = std::fs::read_to_string(&profile_path)?;
    let mut profile = VersionJson::parse(text.as_bytes())?;
    if profile.inherits_from.is_none() {
        profile.inherits_from = Some(minecraft.to_owned());
    }

    Ok(LoaderProfile {
        version_id: profile.id.clone(),
        jvm_args: Vec::new(),
        game_args: Vec::new(),
        profile,
    })
}

/// Create the launcher-profile files the installer requires.
///
/// `launcher_profiles.json` and `launcher_profiles_microsoft_store.json` are
/// only checked for existence; their content is never read by the installer.
fn ensure_launcher_profiles(common_dir: &Path) -> Result<()> {
    let profiles = common_dir.join("launcher_profiles.json");
    if !profiles.exists() {
        let content = serde_json::json!({
            "profiles": {},
            "settings": {},
            "version": 3
        });
        std::fs::write(&profiles, serde_json::to_string_pretty(&content)?)?;
    }

    let store = common_dir.join("launcher_profiles_microsoft_store.json");
    if !store.exists() {
        std::fs::write(&store, "{}")?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forge_full_version_is_not_double_prefixed() {
        assert_eq!(forge_full_version("1.20.1", "47.2.0"), "1.20.1-47.2.0");
        assert_eq!(forge_full_version("1.20.1", "1.20.1-47.2.0"), "1.20.1-47.2.0");
    }

    #[test]
    fn forge_installer_url_matches_maven_layout() {
        assert_eq!(
            forge_installer_url("1.20.1-47.2.0"),
            "https://maven.minecraftforge.net/net/minecraftforge/forge/1.20.1-47.2.0/forge-1.20.1-47.2.0-installer.jar"
        );
    }

    #[test]
    fn neoforge_prefix_drops_the_leading_one_from_1_20_2() {
        assert_eq!(neoforge_prefix("1.20.1").as_deref(), Some("47.1"));
        assert_eq!(neoforge_prefix("1.20.4").as_deref(), Some("20.4"));
        assert_eq!(neoforge_prefix("1.21.1").as_deref(), Some("21.1"));
        assert_eq!(neoforge_prefix("1.21").as_deref(), Some("21.0"));
        assert_eq!(neoforge_prefix("2.0"), None);
    }

    #[test]
    fn neoforge_installer_url_matches_maven_layout() {
        assert_eq!(
            neoforge_installer_url("21.1.92"),
            "https://maven.neoforged.net/releases/net/neoforged/neoforge/21.1.92/neoforge-21.1.92-installer.jar"
        );
    }

    #[test]
    fn game_version_is_stripped_from_either_side() {
        // Modern builds put the game version first.
        assert_eq!(strip_game_version("1.20.1-47.4.29", "1.20.1"), "47.4.29");
        // 1.7.10-era builds wrap it on both sides.
        assert_eq!(strip_game_version("1.7.10-10.13.4.1614-1.7.10", "1.7.10"), "10.13.4.1614");
        // An id without the game version is left alone rather than mangled.
        assert_eq!(strip_game_version("47.4.29", "1.20.1"), "47.4.29");
    }

    #[test]
    fn maven_versions_are_extracted() {
        let xml = r#"<metadata><versioning><versions>
            <version>21.1.90</version>
            <version>21.1.92</version>
            <version>21.2.0-beta</version>
        </versions></versioning></metadata>"#;
        assert_eq!(parse_maven_versions(xml), vec!["21.1.90", "21.1.92", "21.2.0-beta"]);
    }

    #[test]
    fn newest_neoforge_is_selected_by_semver() {
        // Mirrors the selection loop without hitting the network.
        let xml = r#"<metadata><versions>
            <version>21.1.9</version>
            <version>21.1.92</version>
            <version>21.1.100</version>
            <version>21.2.0-beta</version>
        </versions></metadata>"#;
        let prefix = neoforge_prefix("1.21.1").unwrap();

        let mut best: Option<semver::Version> = None;
        for version in parse_maven_versions(xml) {
            if !version.starts_with(&format!("{prefix}.")) {
                continue;
            }
            if let Ok(parsed) = semver::Version::parse(&version)
                && best.as_ref().is_none_or(|current| parsed > *current)
            {
                best = Some(parsed);
            }
        }
        assert_eq!(best.map(|v| v.to_string()).as_deref(), Some("21.1.100"));
    }
}
