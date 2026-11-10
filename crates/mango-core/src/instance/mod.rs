//! Instances: the unit the launcher installs and launches.
//!
//! An instance is "a Minecraft version, a mod loader, and a set of mods". It can
//! come from two places:
//!
//! * a **server pack** declared in a manifest, where the server owner decides
//!   everything, or
//! * a **user instance** the player creates by picking a version and loader.
//!
//! Both reduce to the same [`InstanceSpec`], so one install and launch path
//! serves both and they cannot drift apart.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{MangoError, Result};
use crate::manifest::{FileEntry, JavaSpec, LoaderKind, LoaderSpec, ModEntry, ServerEntry};

/// Everything needed to install and launch one instance.
#[derive(Debug, Clone, PartialEq)]
pub struct InstanceSpec {
    /// Directory name under `instances/`. Stable; renaming orphans player data.
    pub id: String,
    pub name: String,
    pub minecraft: String,
    pub loader: LoaderSpec,
    pub java: Option<JavaSpec>,
    pub jvm_args: Vec<String>,
    pub mods: Vec<ModEntry>,
    pub files: Vec<FileEntry>,
    /// `host:port` to join on launch, when the instance belongs to a server.
    pub address: Option<String>,
    pub auto_connect: bool,
    /// How this instance came to exist.
    pub origin: InstanceOrigin,
}

/// Where an instance's definition came from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum InstanceOrigin {
    /// Declared by a manifest server; the server owns the definition.
    Server { server_id: String },
    /// Created by the player and stored in `instance.json`.
    User,
}

impl InstanceOrigin {
    /// Whether the player may change the version and loader.
    ///
    /// A server pack pins these: the server rejects a mismatched client, so
    /// letting the player edit them would only produce a broken install.
    pub fn is_editable(&self) -> bool {
        matches!(self, InstanceOrigin::User)
    }
}

impl ServerEntry {
    /// Turn a manifest server into the spec the installer consumes.
    pub fn to_instance_spec(&self) -> InstanceSpec {
        InstanceSpec {
            id: self.id.clone(),
            name: self.name.clone(),
            minecraft: self.minecraft.clone(),
            loader: self.loader.clone(),
            java: self.java.clone(),
            jvm_args: self.jvm_args.clone(),
            mods: self.mods.clone(),
            files: self.files.clone(),
            address: Some(self.address.clone()),
            auto_connect: self.auto_connect,
            origin: InstanceOrigin::Server { server_id: self.id.clone() },
        }
    }
}

/// A player-created instance as stored on disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UserInstance {
    pub id: String,
    pub name: String,
    pub minecraft: String,
    #[serde(default)]
    pub loader: LoaderSpec,
    #[serde(default)]
    pub java: Option<JavaSpec>,
    #[serde(default)]
    pub jvm_args: Vec<String>,
    #[serde(default)]
    pub mods: Vec<ModEntry>,
    /// ISO-8601 timestamp, for sorting the list.
    #[serde(default)]
    pub created_at: Option<String>,
}

/// File name of a user instance definition inside its directory.
pub const INSTANCE_FILE: &str = "instance.json";

/// The mutable fields of an instance, as edited by the player.
///
/// Kept separate from [`UserInstance`] so an edit cannot accidentally change the
/// id or creation time.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct InstanceEdit {
    pub name: String,
    pub minecraft: String,
    pub loader: LoaderSpec,
    pub java: Option<JavaSpec>,
    pub jvm_args: Vec<String>,
}

impl From<&UserInstance> for InstanceEdit {
    /// Pre-fill the form from the current definition.
    fn from(instance: &UserInstance) -> Self {
        Self {
            name: instance.name.clone(),
            minecraft: instance.minecraft.clone(),
            loader: instance.loader.clone(),
            java: instance.java.clone(),
            jvm_args: instance.jvm_args.clone(),
        }
    }
}

impl UserInstance {
    /// Create a definition, validating the choices the player made.
    pub fn new(id: &str, name: &str, minecraft: &str, loader: LoaderSpec) -> Result<Self> {
        let id = id.trim();
        if id.is_empty() {
            return Err(MangoError::Config("instance id must not be empty".to_owned()));
        }
        if !is_safe_id(id) {
            return Err(MangoError::Config(format!(
                "instance id `{id}` may only contain letters, digits, `-` and `_`"
            )));
        }
        if minecraft.trim().is_empty() {
            return Err(MangoError::Config("choose a Minecraft version".to_owned()));
        }
        if loader.kind == LoaderKind::Vanilla && loader.version.is_some() {
            return Err(MangoError::Config(
                "a vanilla instance cannot pin a loader version".to_owned(),
            ));
        }

        Ok(Self {
            id: id.to_owned(),
            name: if name.trim().is_empty() { id.to_owned() } else { name.trim().to_owned() },
            minecraft: minecraft.trim().to_owned(),
            loader,
            java: None,
            jvm_args: Vec::new(),
            mods: Vec::new(),
            created_at: Some(now_iso8601()),
        })
    }

    /// Convert to the shared install spec.
    pub fn to_spec(&self) -> InstanceSpec {
        InstanceSpec {
            id: self.id.clone(),
            name: self.name.clone(),
            minecraft: self.minecraft.clone(),
            loader: self.loader.clone(),
            java: self.java.clone(),
            jvm_args: self.jvm_args.clone(),
            mods: self.mods.clone(),
            files: Vec::new(),
            address: None,
            auto_connect: false,
            origin: InstanceOrigin::User,
        }
    }

    /// Apply changes to an existing instance and persist them.
    ///
    /// Only the fields a player may change are touched: the `id` and
    /// `created_at` stay fixed, because renaming the id would orphan the game
    /// directory (worlds, configs) that already lives under it.
    pub fn update(&mut self, edit: InstanceEdit) -> Result<()> {
        let minecraft = edit.minecraft.trim();
        if minecraft.is_empty() {
            return Err(MangoError::Config("choose a Minecraft version".to_owned()));
        }
        if edit.loader.kind == LoaderKind::Vanilla && edit.loader.version.is_some() {
            return Err(MangoError::Config(
                "a vanilla instance cannot pin a loader version".to_owned(),
            ));
        }

        // An empty name falls back to the id, matching `new`.
        let name = edit.name.trim();
        self.name = if name.is_empty() { self.id.clone() } else { name.to_owned() };
        self.minecraft = minecraft.to_owned();
        self.loader = edit.loader;
        self.java = edit.java;
        self.jvm_args = edit.jvm_args;
        Ok(())
    }

    pub fn save(&self, instances_dir: &Path) -> Result<PathBuf> {
        let dir = instances_dir.join(&self.id);
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(INSTANCE_FILE);
        let mut text = serde_json::to_string_pretty(self)?;
        text.push('\n');
        std::fs::write(&path, text)?;
        Ok(path)
    }

    pub fn load(instance_dir: &Path) -> Result<Option<Self>> {
        let path = instance_dir.join(INSTANCE_FILE);
        if !path.exists() {
            return Ok(None);
        }
        let text = std::fs::read_to_string(&path)?;
        Ok(Some(serde_json::from_str(strip_bom(&text))?))
    }

    /// Delete the definition and the whole instance directory.
    ///
    /// Callers must confirm with the player: this removes worlds and configs, not
    /// just the recipe.
    pub fn delete(instances_dir: &Path, id: &str) -> Result<()> {
        if !is_safe_id(id) {
            return Err(MangoError::Config(format!("refusing to delete unsafe id `{id}`")));
        }
        let dir = instances_dir.join(id);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }
}

/// List every user instance under an instances directory.
///
/// Unreadable or malformed definitions are skipped rather than aborting the
/// list, so one broken instance does not hide all the others.
pub fn list_user_instances(instances_dir: &Path) -> Vec<UserInstance> {
    let Ok(entries) = std::fs::read_dir(instances_dir) else { return Vec::new() };

    let mut found: Vec<UserInstance> = entries
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter_map(|entry| match UserInstance::load(&entry.path()) {
            Ok(Some(instance)) => Some(instance),
            Ok(None) => None,
            Err(err) => {
                tracing::warn!(
                    path = %entry.path().display(),
                    error = %err,
                    "skipping unreadable instance"
                );
                None
            }
        })
        .collect();

    // Case-insensitive so "Zebra" and "apple" sort as a player expects.
    found.sort_by_key(|instance| instance.name.to_lowercase());
    found
}

/// Suggest an instance id from a display name, avoiding collisions.
pub fn suggest_id(name: &str, existing: &[String]) -> String {
    let base: String = name
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    let base = if base.is_empty() { "instance".to_owned() } else { base };
    if !existing.iter().any(|id| id == &base) {
        return base;
    }

    for suffix in 2..1000 {
        let candidate = format!("{base}-{suffix}");
        if !existing.iter().any(|id| id == &candidate) {
            return candidate;
        }
    }
    base
}

fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

fn strip_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

fn now_iso8601() -> String {
    // Second precision is plenty for ordering a list of instances.
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format_iso8601(now)
}

fn format_iso8601(unix_seconds: u64) -> String {
    // Avoid a chrono dependency here: days-since-epoch arithmetic is enough.
    let days = unix_seconds / 86_400;
    let seconds_in_day = unix_seconds % 86_400;
    let (year, month, day) = civil_from_days(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds_in_day / 3600,
        (seconds_in_day % 3600) / 60,
        seconds_in_day % 60
    )
}

/// Howard Hinnant's days-to-civil algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mango-inst-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn server_entries_convert_to_specs() {
        let server: ServerEntry = serde_json::from_str(
            r#"{"id":"main","name":"Main","address":"play.example.com:25565","minecraft":"1.20.1",
                "autoConnect":true,"loader":{"type":"fabric","version":"0.16.9"},
                "mods":[{"source":"modrinth","id":"sodium"}]}"#,
        )
        .unwrap();

        let spec = server.to_instance_spec();
        assert_eq!(spec.id, "main");
        assert_eq!(spec.minecraft, "1.20.1");
        assert_eq!(spec.address.as_deref(), Some("play.example.com:25565"));
        assert!(spec.auto_connect);
        assert_eq!(spec.mods.len(), 1);
        assert_eq!(spec.origin, InstanceOrigin::Server { server_id: "main".to_owned() });
        assert!(!spec.origin.is_editable());
    }

    #[test]
    fn user_instance_ids_are_validated() {
        let loader = LoaderSpec { kind: LoaderKind::Fabric, version: None };
        assert!(UserInstance::new("my-instance", "Mine", "1.20.1", loader.clone()).is_ok());
        assert!(UserInstance::new("My Instance", "Mine", "1.20.1", loader.clone()).is_err());
        assert!(UserInstance::new("../escape", "Mine", "1.20.1", loader.clone()).is_err());
        assert!(UserInstance::new("", "Mine", "1.20.1", loader).is_err());
    }

    #[test]
    fn vanilla_instance_cannot_pin_a_loader_version() {
        let loader = LoaderSpec { kind: LoaderKind::Vanilla, version: Some("1.0".to_owned()) };
        assert!(UserInstance::new("a", "A", "1.20.1", loader).is_err());
    }

    #[test]
    fn empty_name_falls_back_to_the_id() {
        let loader = LoaderSpec::default();
        let instance = UserInstance::new("abc", "  ", "1.20.1", loader).unwrap();
        assert_eq!(instance.name, "abc");
    }

    #[test]
    fn user_instances_round_trip_through_disk() {
        let dir = temp_dir("roundtrip");
        let loader = LoaderSpec { kind: LoaderKind::Fabric, version: Some("0.16.9".to_owned()) };
        let mut instance = UserInstance::new("mine", "My Pack", "1.20.1", loader).unwrap();
        instance.mods.push(ModEntry {
            source: crate::manifest::ModSource::Modrinth,
            id: "sodium".to_owned(),
            version: None,
            file_id: None,
            required: true,
            disabled_by_default: false,
        });

        instance.save(&dir).unwrap();
        let loaded = UserInstance::load(&dir.join("mine")).unwrap().unwrap();
        assert_eq!(loaded, instance);

        let listed = list_user_instances(&dir);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "mine");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_definition_is_not_an_error() {
        let dir = temp_dir("missing");
        assert!(UserInstance::load(&dir).unwrap().is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn broken_definitions_are_skipped_when_listing() {
        let dir = temp_dir("broken");
        let good = UserInstance::new("good", "Good", "1.20.1", LoaderSpec::default()).unwrap();
        good.save(&dir).unwrap();

        let broken = dir.join("broken");
        std::fs::create_dir_all(&broken).unwrap();
        std::fs::write(broken.join(INSTANCE_FILE), "{not json").unwrap();

        let listed = list_user_instances(&dir);
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "good");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn updating_changes_only_the_editable_fields() {
        let loader = LoaderSpec { kind: LoaderKind::Fabric, version: Some("0.16.9".to_owned()) };
        let mut instance = UserInstance::new("keep-id", "Old", "1.20.1", loader).unwrap();
        let created = instance.created_at.clone();

        instance
            .update(InstanceEdit {
                name: "New Name".to_owned(),
                minecraft: "1.21.1".to_owned(),
                loader: LoaderSpec {
                    kind: LoaderKind::Neoforge,
                    version: Some("21.1.92".to_owned()),
                },
                java: None,
                jvm_args: vec!["-Xmx4G".to_owned()],
            })
            .unwrap();

        assert_eq!(instance.name, "New Name");
        assert_eq!(instance.minecraft, "1.21.1");
        assert_eq!(instance.loader.kind, LoaderKind::Neoforge);
        assert_eq!(instance.loader.version.as_deref(), Some("21.1.92"));
        assert_eq!(instance.jvm_args, vec!["-Xmx4G".to_owned()]);

        // Identity must not change: the directory name and history depend on it.
        assert_eq!(instance.id, "keep-id");
        assert_eq!(instance.created_at, created);
    }

    #[test]
    fn an_empty_name_on_edit_falls_back_to_the_id() {
        let mut instance =
            UserInstance::new("my-pack", "My Pack", "1.20.1", LoaderSpec::default()).unwrap();

        instance
            .update(InstanceEdit {
                name: "   ".to_owned(),
                minecraft: "1.20.1".to_owned(),
                ..Default::default()
            })
            .unwrap();

        assert_eq!(instance.name, "my-pack");
    }

    #[test]
    fn editing_rejects_invalid_combinations() {
        let mut instance = UserInstance::new("a", "A", "1.20.1", LoaderSpec::default()).unwrap();

        // No Minecraft version.
        let error = instance
            .update(InstanceEdit { minecraft: "  ".to_owned(), ..Default::default() })
            .unwrap_err();
        assert!(error.to_string().contains("Minecraft version"), "{error}");
        // The instance is untouched by a rejected edit.
        assert_eq!(instance.minecraft, "1.20.1");

        // Vanilla cannot pin a loader version.
        let error = instance
            .update(InstanceEdit {
                minecraft: "1.20.1".to_owned(),
                loader: LoaderSpec { kind: LoaderKind::Vanilla, version: Some("1.0".to_owned()) },
                ..Default::default()
            })
            .unwrap_err();
        assert!(error.to_string().contains("vanilla"), "{error}");
    }

    #[test]
    fn edit_is_prefilled_from_the_instance() {
        let loader = LoaderSpec { kind: LoaderKind::Forge, version: Some("47.4.29".to_owned()) };
        let instance = UserInstance::new("a", "Alpha", "1.20.1", loader.clone()).unwrap();

        let edit = InstanceEdit::from(&instance);
        assert_eq!(edit.name, "Alpha");
        assert_eq!(edit.minecraft, "1.20.1");
        assert_eq!(edit.loader, loader);
    }

    #[test]
    fn edited_instance_round_trips_through_disk() {
        let dir = temp_dir("edit-roundtrip");
        let mut instance =
            UserInstance::new("a", "Alpha", "1.20.1", LoaderSpec::default()).unwrap();
        instance.save(&dir).unwrap();

        instance
            .update(InstanceEdit {
                name: "Renamed".to_owned(),
                minecraft: "1.21.1".to_owned(),
                loader: LoaderSpec { kind: LoaderKind::Fabric, version: None },
                ..Default::default()
            })
            .unwrap();
        instance.save(&dir).unwrap();

        let loaded = UserInstance::load(&dir.join("a")).unwrap().unwrap();
        assert_eq!(loaded.name, "Renamed");
        assert_eq!(loaded.minecraft, "1.21.1");
        assert_eq!(loaded.loader.kind, LoaderKind::Fabric);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn deleting_refuses_unsafe_ids() {
        let dir = temp_dir("delete");
        assert!(UserInstance::delete(&dir, "../etc").is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn deleting_removes_the_whole_directory() {
        let dir = temp_dir("delete-ok");
        let instance = UserInstance::new("gone", "Gone", "1.20.1", LoaderSpec::default()).unwrap();
        instance.save(&dir).unwrap();
        std::fs::write(dir.join("gone").join("world.dat"), b"x").unwrap();

        UserInstance::delete(&dir, "gone").unwrap();
        assert!(!dir.join("gone").exists());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn ids_avoid_collisions() {
        let existing = vec!["my-pack".to_owned(), "my-pack-2".to_owned()];
        assert_eq!(suggest_id("My Pack", &existing), "my-pack-3");
        assert_eq!(suggest_id("Fresh", &existing), "fresh");
        assert_eq!(suggest_id("   ", &[]), "instance");
        assert_eq!(suggest_id("Hello, World!", &[]), "hello-world");
    }

    #[test]
    fn user_specs_are_editable() {
        let instance = UserInstance::new("a", "A", "1.20.1", LoaderSpec::default()).unwrap();
        assert!(instance.to_spec().origin.is_editable());
    }

    #[test]
    fn timestamps_are_iso8601() {
        // 2026-10-30T00:00:00Z, verified against `date -u -d @1793318400`.
        assert_eq!(format_iso8601(1_793_318_400), "2026-10-30T00:00:00Z");
        assert_eq!(format_iso8601(0), "1970-01-01T00:00:00Z");
        // A leap day, which is where naive month arithmetic usually breaks.
        assert_eq!(format_iso8601(1_709_164_800), "2024-02-29T00:00:00Z");
        // A padded single-digit month and day.
        assert_eq!(format_iso8601(1_704_067_200), "2024-01-01T00:00:00Z");
    }
}
