//! Persistent launcher configuration.
//!
//! The schema mirrors the settings Helios Launcher exposes (game resolution,
//! fullscreen, autoconnect, per-server Java options, mod toggles) but drops the
//! token blobs: credentials live in the OS keyring, so `config.json` only holds
//! non-sensitive account metadata.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;
use crate::paths::DataPaths;

const GIB: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    pub settings: Settings,
    pub news_cache: NewsCache,
    /// Server id currently selected in the UI.
    pub selected_server: Option<String>,
    /// Account uuid currently selected in the UI.
    pub selected_account: Option<String>,
    /// Non-sensitive account metadata, keyed by uuid. Tokens live in the keyring.
    pub accounts: BTreeMap<String, AccountMeta>,
    pub mod_configurations: Vec<ModConfiguration>,
    pub java_config: BTreeMap<String, JavaConfig>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    pub game: GameSettings,
    pub launcher: LauncherSettings,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct GameSettings {
    pub res_width: u32,
    pub res_height: u32,
    pub fullscreen: bool,
    pub auto_connect: bool,
    pub launch_detached: bool,
}

impl Default for GameSettings {
    fn default() -> Self {
        Self {
            res_width: 1280,
            res_height: 720,
            fullscreen: false,
            auto_connect: true,
            launch_detached: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct LauncherSettings {
    pub allow_prerelease: bool,
    /// Root of the game installation. Falls back to the launcher data root.
    pub data_directory: Option<String>,
    pub enable_discord_rpc: bool,
    /// Manifest path or URL the launcher loads at startup.
    pub manifest_url: Option<String>,
}

impl Default for LauncherSettings {
    fn default() -> Self {
        Self {
            allow_prerelease: false,
            data_directory: None,
            enable_discord_rpc: true,
            manifest_url: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct NewsCache {
    pub date: Option<String>,
    pub content: Option<String>,
    pub dismissed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountMeta {
    pub kind: AccountKind,
    pub uuid: String,
    pub username: String,
    /// Unix milliseconds when the Minecraft access token expires.
    #[serde(default)]
    pub expires_at: Option<i64>,
    /// Base URL of a custom Yggdrasil server, when not Microsoft.
    #[serde(default)]
    pub yggdrasil_base: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AccountKind {
    Microsoft,
    Yggdrasil,
    Offline,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModConfiguration {
    pub id: String,
    /// `true`/`false` for a leaf toggle, or `{ "value": bool, "mods": { .. } }`
    /// for a module with sub-modules.
    #[serde(default)]
    pub mods: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct JavaConfig {
    pub min_ram: String,
    pub max_ram: String,
    pub executable: Option<String>,
    pub jvm_options: Vec<String>,
}

impl Default for JavaConfig {
    fn default() -> Self {
        Self {
            min_ram: "2G".to_owned(),
            max_ram: "2G".to_owned(),
            executable: None,
            jvm_options: Vec::new(),
        }
    }
}

impl Config {
    /// Load from disk, creating a default file when absent or unreadable.
    ///
    /// Malformed JSON is never fatal: it is logged and replaced, matching the
    /// launcher's "always start" contract.
    pub fn load(paths: &DataPaths) -> Result<Self> {
        paths.ensure()?;
        let path = paths.config_file();

        if !path.exists() {
            let config = Config::default();
            config.save(paths)?;
            return Ok(config);
        }

        let raw = std::fs::read_to_string(&path)?;
        match serde_json::from_str::<Config>(strip_bom(&raw)) {
            Ok(config) => Ok(config),
            Err(err) => {
                tracing::warn!(error = %err, "config file is malformed, regenerating defaults");
                let config = Config::default();
                config.save(paths)?;
                Ok(config)
            }
        }
    }

    pub fn save(&self, paths: &DataPaths) -> Result<()> {
        paths.ensure()?;
        write_json(&paths.config_file(), self)
    }

    /// Per-server Java settings, created on first use from the server's RAM hint.
    pub fn ensure_java_config(
        &mut self,
        server_id: &str,
        suggested_major: u32,
        total_ram: u64,
        ram: Option<&RamHint>,
    ) -> &JavaConfig {
        self.java_config.entry(server_id.to_owned()).or_insert_with(|| {
            let ram_value = resolve_selected_ram(total_ram, ram);
            JavaConfig {
                min_ram: ram_value.clone(),
                max_ram: ram_value,
                executable: None,
                jvm_options: default_jvm_options(suggested_major),
            }
        })
    }

    pub fn mod_configuration(&self, server_id: &str) -> Option<&ModConfiguration> {
        self.mod_configurations.iter().find(|c| c.id == server_id)
    }

    /// Insert or replace the mod configuration for a server.
    pub fn set_mod_configuration(&mut self, server_id: &str, mods: Value) {
        match self.mod_configurations.iter_mut().find(|c| c.id == server_id) {
            Some(existing) => existing.mods = mods,
            None => {
                self.mod_configurations.push(ModConfiguration { id: server_id.to_owned(), mods })
            }
        }
    }

    /// Remove an account and pick a replacement selection if needed.
    pub fn remove_account(&mut self, uuid: &str) {
        self.accounts.remove(uuid);
        if self.selected_account.as_deref() == Some(uuid) {
            self.selected_account = self.accounts.keys().next().cloned();
        }
    }
}

/// Recommended/minimum RAM, in megabytes, declared by a server manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RamHint {
    pub recommended: u32,
    pub minimum: u32,
}

/// Smallest value the RAM slider offers, in gigabytes.
///
/// Falls back to the legacy heuristic (3 GB on machines with 6 GB or more) when
/// the server does not declare a minimum.
pub fn absolute_min_ram_gb(total_ram: u64, ram: Option<&RamHint>) -> u32 {
    match ram {
        Some(hint) => (hint.minimum / 1024).max(1),
        None => {
            if total_ram >= 6 * GIB {
                3
            } else {
                2
            }
        }
    }
}

/// Largest value the RAM slider offers, in gigabytes.
///
/// Keeps a quarter of installed memory free, plus a slice of anything above
/// 16 GB, so the game never starves the operating system.
pub fn absolute_max_ram_gb(total_ram: u64) -> u32 {
    let above_16 = total_ram.saturating_sub(16 * GIB);
    let reserved = if above_16 > 0 { above_16 / 8 + 16 * GIB / 4 } else { total_ram / 4 };
    (total_ram.saturating_sub(reserved) / GIB) as u32
}

/// Default `-Xmx`/`-Xms` value.
pub fn resolve_selected_ram(total_ram: u64, ram: Option<&RamHint>) -> String {
    match ram {
        Some(hint) => format!("{}M", hint.recommended),
        None => {
            if total_ram >= 8 * GIB {
                "4G".to_owned()
            } else if total_ram >= 6 * GIB {
                "3G".to_owned()
            } else {
                "2G".to_owned()
            }
        }
    }
}

/// Default GC flags, split on the Java 8 boundary because CMS is gone in newer VMs.
pub fn default_jvm_options(suggested_major: u32) -> Vec<String> {
    let options: &[&str] = if suggested_major > 8 {
        &[
            "-XX:+UnlockExperimentalVMOptions",
            "-XX:+UseG1GC",
            "-XX:G1NewSizePercent=20",
            "-XX:G1ReservePercent=20",
            "-XX:MaxGCPauseMillis=50",
            "-XX:G1HeapRegionSize=32M",
        ]
    } else {
        &[
            "-XX:+UseConcMarkSweepGC",
            "-XX:+CMSIncrementalMode",
            "-XX:-UseAdaptiveSizePolicy",
            "-Xmn128M",
        ]
    };
    options.iter().map(|s| (*s).to_owned()).collect()
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let mut json = serde_json::to_string_pretty(value)?;
    json.push('\n');
    std::fs::write(path, json)?;
    Ok(())
}

/// Strip a UTF-8 byte-order mark.
///
/// Editors and shells on Windows write BOM-prefixed UTF-8, and `serde_json`
/// rejects a leading BOM rather than skipping it, so a hand-edited config would
/// otherwise look corrupt.
fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_legacy_launcher() {
        let config = Config::default();
        assert_eq!(config.settings.game.res_width, 1280);
        assert_eq!(config.settings.game.res_height, 720);
        assert!(config.settings.game.auto_connect);
        assert!(config.settings.game.launch_detached);
        assert!(!config.settings.launcher.allow_prerelease);
    }

    #[test]
    fn missing_fields_are_filled_from_defaults() {
        let parsed: Config =
            serde_json::from_str(r#"{"settings":{"game":{"resWidth":2560}}}"#).unwrap();
        assert_eq!(parsed.settings.game.res_width, 2560);
        assert_eq!(parsed.settings.game.res_height, 720);
        assert!(parsed.settings.game.launch_detached);
    }

    #[test]
    fn ram_heuristics_match_legacy_values() {
        assert_eq!(absolute_max_ram_gb(8 * GIB), 6);
        assert_eq!(absolute_max_ram_gb(16 * GIB), 12);
        assert_eq!(absolute_min_ram_gb(8 * GIB, None), 3);
        assert_eq!(absolute_min_ram_gb(4 * GIB, None), 2);
        assert_eq!(resolve_selected_ram(16 * GIB, None), "4G");
        assert_eq!(resolve_selected_ram(6 * GIB, None), "3G");
        assert_eq!(resolve_selected_ram(4 * GIB, None), "2G");
    }

    #[test]
    fn server_ram_hint_wins_over_heuristics() {
        let hint = RamHint { recommended: 3072, minimum: 2048 };
        assert_eq!(resolve_selected_ram(32 * GIB, Some(&hint)), "3072M");
        assert_eq!(absolute_min_ram_gb(32 * GIB, Some(&hint)), 2);
    }

    #[test]
    fn java_config_created_once() {
        let mut config = Config::default();
        config.ensure_java_config("main", 17, 16 * GIB, None);
        config.java_config.get_mut("main").unwrap().max_ram = "6G".to_owned();
        config.ensure_java_config("main", 17, 16 * GIB, None);
        assert_eq!(config.java_config["main"].max_ram, "6G");
    }

    #[test]
    fn jvm_options_split_on_java_8() {
        assert!(default_jvm_options(8).contains(&"-XX:+UseConcMarkSweepGC".to_owned()));
        assert!(default_jvm_options(17).contains(&"-XX:+UseG1GC".to_owned()));
    }

    #[test]
    fn removing_selected_account_picks_another() {
        let mut config = Config::default();
        let meta = |uuid: &str| AccountMeta {
            kind: AccountKind::Offline,
            uuid: uuid.to_owned(),
            username: uuid.to_owned(),
            expires_at: None,
            yggdrasil_base: None,
        };
        config.accounts.insert("a".into(), meta("a"));
        config.accounts.insert("b".into(), meta("b"));
        config.selected_account = Some("a".into());
        config.remove_account("a");
        assert_eq!(config.selected_account.as_deref(), Some("b"));
    }

    #[test]
    fn config_round_trips_through_disk() {
        let dir = std::env::temp_dir().join(format!("mango-cfg-{}", std::process::id()));
        let paths = DataPaths::new(&dir);
        let mut config = Config::default();
        config.settings.game.res_width = 1920;
        config.save(&paths).unwrap();

        let loaded = Config::load(&paths).unwrap();
        assert_eq!(loaded.settings.game.res_width, 1920);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn malformed_config_is_replaced() {
        let dir = std::env::temp_dir().join(format!("mango-bad-cfg-{}", std::process::id()));
        let paths = DataPaths::new(&dir);
        paths.ensure().unwrap();
        std::fs::write(paths.config_file(), "{ this is not json").unwrap();

        let loaded = Config::load(&paths).unwrap();
        assert_eq!(loaded.settings.game.res_width, 1280);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn byte_order_mark_is_tolerated() {
        let dir = std::env::temp_dir().join(format!("mango-bom-cfg-{}", std::process::id()));
        let paths = DataPaths::new(&dir);
        paths.ensure().unwrap();
        std::fs::write(
            paths.config_file(),
            "\u{feff}{\"settings\":{\"game\":{\"resWidth\":2560}}}",
        )
        .unwrap();

        let loaded = Config::load(&paths).unwrap();
        assert_eq!(loaded.settings.game.res_width, 2560);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn manifest_url_is_persisted() {
        let dir = std::env::temp_dir().join(format!("mango-manifest-cfg-{}", std::process::id()));
        let paths = DataPaths::new(&dir);

        let mut config = Config::default();
        assert_eq!(config.settings.launcher.manifest_url, None);
        config.settings.launcher.manifest_url = Some("https://example.com/m.json".to_owned());
        config.save(&paths).unwrap();

        let loaded = Config::load(&paths).unwrap();
        assert_eq!(
            loaded.settings.launcher.manifest_url.as_deref(),
            Some("https://example.com/m.json")
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
