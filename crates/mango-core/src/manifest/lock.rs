//! The install lockfile.
//!
//! A lockfile pins every file an installation resolved to, including the exact
//! upstream version of each mod. Without it, `latest` would drift between two
//! installs of the same manifest — which breaks reproducing a bug report and
//! makes a shared server pack unstable.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// Lockfile name inside the instance directory.
pub const LOCKFILE_NAME: &str = "mango.lock.json";
/// Format version this build writes.
pub const LOCKFILE_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Lockfile {
    pub schema_version: u32,
    pub server: String,
    /// `updatedAt` of the manifest this lock was generated from.
    #[serde(default)]
    pub manifest_version: Option<String>,
    pub minecraft: String,
    pub loader: LockedLoader,
    /// Absolute path of the `version.json` used, so a future resolve can skip
    /// the version manifest round-trip.
    #[serde(default)]
    pub version_url: Option<String>,
    /// Every file required, with its hash.
    pub files: Vec<LockedFile>,
    /// Resolved mods, so the UI can show versions without hitting the network.
    #[serde(default)]
    pub mods: Vec<LockedMod>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LockedLoader {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LockedFile {
    pub kind: String,
    /// Which root `path` is relative to.
    pub scope: FileScope,
    /// Path relative to the scope root, always with `/` separators so a
    /// lockfile written on Windows is readable on Linux.
    pub path: String,
    pub url: String,
    pub size: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha1: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha512: Option<String>,
}

/// Which directory a locked file lives under.
///
/// Files are split by root rather than stored relative to a single game root so
/// a lockfile stays valid when the player relocates only one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileScope {
    /// Shared across servers: libraries, assets, version jars.
    Common,
    /// Per-server: mods, configs, and anything inside the instance directory.
    Instance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LockedMod {
    pub source: String,
    pub id: String,
    pub version_id: String,
    pub file_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha512: Option<String>,
    pub required: bool,
}

impl Lockfile {
    /// Path of the lockfile for an instance directory.
    pub fn path_for(instance_dir: &Path) -> PathBuf {
        instance_dir.join(LOCKFILE_NAME)
    }

    pub fn load(instance_dir: &Path) -> Result<Option<Self>> {
        let path = Self::path_for(instance_dir);
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)?;
        Ok(Some(serde_json::from_str(&text)?))
    }

    pub fn save(&self, instance_dir: &Path) -> Result<PathBuf> {
        std::fs::create_dir_all(instance_dir)?;
        let path = Self::path_for(instance_dir);
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        std::fs::write(&path, text)?;
        Ok(path)
    }

    /// Whether this lockfile was produced from the same manifest revision.
    pub fn matches_manifest(&self, manifest_version: Option<&str>) -> bool {
        self.manifest_version.as_deref() == manifest_version
    }

    /// Absolute path of a locked file, given the two scope roots.
    pub fn resolve_path(common_dir: &Path, instance_dir: &Path, locked: &LockedFile) -> PathBuf {
        let base = match locked.scope {
            FileScope::Common => common_dir,
            FileScope::Instance => instance_dir,
        };
        let mut path = base.to_path_buf();
        for segment in locked.path.split('/') {
            path.push(segment);
        }
        path
    }
}

/// Convert an absolute path into a lockfile-relative path.
///
/// Returns `None` when `path` is outside `root`, which only happens with a
/// misconfigured data directory.
pub fn relative_path(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let text = relative.to_string_lossy().replace('\\', "/");
    if text.is_empty() { None } else { Some(text) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Lockfile {
        Lockfile {
            schema_version: LOCKFILE_SCHEMA_VERSION,
            server: "main".to_owned(),
            manifest_version: Some("2026-11-09T00:00:00Z".to_owned()),
            minecraft: "1.20.1".to_owned(),
            loader: LockedLoader { kind: "fabric".to_owned(), version: Some("0.16.9".to_owned()) },
            version_url: Some("https://example.com/1.20.1.json".to_owned()),
            files: vec![LockedFile {
                kind: "library".to_owned(),
                scope: FileScope::Common,
                path: "libraries/org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1.jar".to_owned(),
                url: "https://example.com/lwjgl.jar".to_owned(),
                size: 10,
                sha1: Some("abc".to_owned()),
                sha256: None,
                sha512: None,
            }],
            mods: vec![LockedMod {
                source: "modrinth".to_owned(),
                id: "sodium".to_owned(),
                version_id: "mc1.20.1-0.5.11".to_owned(),
                file_name: "sodium-0.5.11.jar".to_owned(),
                sha512: Some("def".to_owned()),
                required: true,
            }],
        }
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("mango-lock-{}", std::process::id()));
        let lock = sample();
        lock.save(&dir).unwrap();

        let loaded = Lockfile::load(&dir).unwrap().unwrap();
        assert_eq!(loaded, lock);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_lockfile_is_not_an_error() {
        let dir = std::env::temp_dir().join(format!("mango-lock-missing-{}", std::process::id()));
        assert_eq!(Lockfile::load(&dir).unwrap(), None);
    }

    #[test]
    fn paths_use_forward_slashes_and_resolve_portably() {
        let common = Path::new("/games/common");
        let instance = Path::new("/games/instances/main");
        let locked = &sample().files[0];
        let resolved = Lockfile::resolve_path(common, instance, locked);
        assert!(resolved.starts_with(common));
        assert_eq!(resolved.file_name().unwrap(), std::ffi::OsStr::new("lwjgl-3.3.1.jar"));

        let relative = relative_path(common, &resolved).unwrap();
        assert_eq!(relative, locked.path);
        assert!(!relative.contains('\\'));
    }

    #[test]
    fn instance_scoped_files_resolve_under_the_instance() {
        let common = Path::new("/games/common");
        let instance = Path::new("/games/instances/main");
        let locked = LockedFile {
            kind: "mod".to_owned(),
            scope: FileScope::Instance,
            path: "mods/sodium.jar".to_owned(),
            url: "https://example.com/sodium.jar".to_owned(),
            size: 1,
            sha1: None,
            sha256: None,
            sha512: Some("def".to_owned()),
        };
        assert_eq!(
            Lockfile::resolve_path(common, instance, &locked),
            instance.join("mods").join("sodium.jar")
        );
    }

    #[test]
    fn manifest_version_comparison_detects_drift() {
        let lock = sample();
        assert!(lock.matches_manifest(Some("2026-11-09T00:00:00Z")));
        assert!(!lock.matches_manifest(Some("2026-11-10T00:00:00Z")));
        assert!(!lock.matches_manifest(None));
    }

    #[test]
    fn paths_outside_root_have_no_relative_form() {
        assert_eq!(relative_path(Path::new("/games"), Path::new("/other/x.jar")), None);
        assert_eq!(relative_path(Path::new("/games"), Path::new("/games")), None);
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let text = r#"{"schemaVersion":1,"server":"a","minecraft":"1.20.1",
            "loader":{"type":"vanilla"},"files":[],"surprise":1}"#;
        assert!(serde_json::from_str::<Lockfile>(text).is_err());
    }
}
