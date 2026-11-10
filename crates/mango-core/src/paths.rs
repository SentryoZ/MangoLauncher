use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{MangoError, Result};

/// Environment variable that overrides the launcher's root directory.
pub const DATA_DIR_ENV: &str = "MANGO_DATA_DIR";

/// Resolve the default launcher root directory for the current platform.
pub fn default_root() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(DATA_DIR_ENV) {
        return Ok(PathBuf::from(path));
    }

    let base = directories::BaseDirs::new().ok_or(MangoError::NoDataDir)?;
    Ok(base.data_dir().join("MangoLauncher"))
}

/// Root of the launcher's own data: config, logs and caches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataPaths {
    root: PathBuf,
}

impl DataPaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Default root, overridable with `MANGO_DATA_DIR`.
    pub fn from_env() -> Result<Self> {
        Ok(Self::new(default_root()?))
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn config_file(&self) -> PathBuf {
        self.root.join("config.json")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }

    pub fn manifest_cache(&self) -> PathBuf {
        self.root.join("manifest.cache.json")
    }

    pub fn ensure(&self) -> Result<()> {
        std::fs::create_dir_all(&self.root)?;
        Ok(())
    }
}

/// Root of the game installation: `common/`, `instances/`, `java/`.
///
/// This is the `settings.launcher.dataDirectory` value and may differ from the
/// [`DataPaths`] root (users can move their install to another drive).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GamePaths {
    root: PathBuf,
}

impl GamePaths {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn common_dir(&self) -> PathBuf {
        self.root.join("common")
    }

    pub fn instances_dir(&self) -> PathBuf {
        self.root.join("instances")
    }

    pub fn instance_dir(&self, server_id: &str) -> PathBuf {
        self.instances_dir().join(server_id)
    }

    pub fn assets_dir(&self) -> PathBuf {
        self.common_dir().join("assets")
    }

    pub fn libraries_dir(&self) -> PathBuf {
        self.common_dir().join("libraries")
    }

    pub fn versions_dir(&self) -> PathBuf {
        self.common_dir().join("versions")
    }

    pub fn java_dir(&self) -> PathBuf {
        self.root.join("java")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_paths_are_stable() {
        let paths = GamePaths::new("/games/mango");
        assert_eq!(paths.common_dir(), PathBuf::from("/games/mango/common"));
        assert_eq!(paths.assets_dir(), PathBuf::from("/games/mango/common/assets"));
        assert_eq!(paths.instance_dir("main"), PathBuf::from("/games/mango/instances/main"));
    }

    #[test]
    fn data_paths_resolve_config_and_logs() {
        let paths = DataPaths::new("/data/mango");
        assert_eq!(paths.config_file(), PathBuf::from("/data/mango/config.json"));
        assert_eq!(paths.logs_dir(), PathBuf::from("/data/mango/logs"));
    }
}
