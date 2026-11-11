//! The MangoLauncher manifest format.
//!
//! A manifest describes one or more servers: which Minecraft version they run,
//! which mod loader, which Java, and which extra mods and files to install.
//! It deliberately differs from Helios' `distribution.json`: mods are
//! referenced by their upstream project id rather than hosted by the launcher,
//! and loader metadata comes from the loader's own API.
//!
//! ```json
//! {
//!   "schemaVersion": 1,
//!   "name": "Mango Network",
//!   "servers": [
//!     {
//!       "id": "main",
//!       "name": "Mango Survival",
//!       "address": "play.example.com:25565",
//!       "minecraft": "1.20.1",
//!       "mainServer": true,
//!       "loader": { "type": "fabric", "version": "0.16.9" }
//!     }
//!   ]
//! }
//! ```

use serde::{Deserialize, Serialize};

use crate::error::{MangoError, Result};

pub mod fetch;
pub mod lock;

pub use fetch::{FetchedManifest, ManifestOrigin, cache_path_for, fetch, load_local};
pub use lock::{FileScope, LockedFile, LockedLoader, LockedMod, Lockfile};

/// Format version this build understands.
pub const SUPPORTED_SCHEMA_VERSION: u32 = 1;

/// A whole manifest document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    pub schema_version: u32,
    pub name: String,
    #[serde(default)]
    pub news: Option<NewsFeed>,
    #[serde(default)]
    pub discord: Option<DiscordPresence>,
    pub servers: Vec<ServerEntry>,
    /// Timestamp of the last update, used by the lockfile to detect drift.
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NewsFeed {
    pub rss: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscordPresence {
    pub client_id: String,
    #[serde(default)]
    pub large_image_key: Option<String>,
    #[serde(default)]
    pub large_image_text: Option<String>,
    #[serde(default)]
    pub small_image_key: Option<String>,
    #[serde(default)]
    pub small_image_text: Option<String>,
}

/// One playable server and its full installation recipe.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerEntry {
    /// Stable identifier. Changing it orphans the player's instance directory.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    /// `host` or `host:port`.
    pub address: String,
    /// Minecraft version id, e.g. `1.20.1`.
    pub minecraft: String,
    #[serde(default)]
    pub main_server: bool,
    #[serde(default)]
    pub auto_connect: bool,
    #[serde(default)]
    pub loader: LoaderSpec,
    #[serde(default)]
    pub java: Option<JavaSpec>,
    /// Extra JVM flags, applied in addition to the launcher's defaults.
    #[serde(default)]
    pub jvm_args: Vec<String>,
    #[serde(default)]
    pub mods: Vec<ModEntry>,
    /// Loose files placed into the instance directory.
    #[serde(default)]
    pub files: Vec<FileEntry>,
}

impl ServerEntry {
    /// Split `host[:port]`, defaulting to the Minecraft port.
    pub fn host_port(&self) -> (String, u16) {
        match self.address.rsplit_once(':') {
            Some((host, port)) => match port.parse::<u16>() {
                Ok(port) => (host.to_owned(), port),
                // Not a port (e.g. an IPv6 address without brackets): keep the
                // whole address and use the default port.
                Err(_) => (self.address.clone(), 25565),
            },
            None => (self.address.clone(), 25565),
        }
    }
}

/// Which mod loader, if any, the server needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoaderSpec {
    #[serde(rename = "type", default)]
    pub kind: LoaderKind,
    /// Loader version; empty means "newest compatible with the game version".
    #[serde(default)]
    pub version: Option<String>,
}

impl Default for LoaderSpec {
    fn default() -> Self {
        Self { kind: LoaderKind::Vanilla, version: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LoaderKind {
    #[default]
    Vanilla,
    Fabric,
    Quilt,
    Forge,
    Neoforge,
}

impl LoaderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Vanilla => "vanilla",
            Self::Fabric => "fabric",
            Self::Quilt => "quilt",
            Self::Forge => "forge",
            Self::Neoforge => "neoforge",
        }
    }

    /// Loaders that publish a ready-to-use `version.json` profile.
    pub fn has_profile_json(self) -> bool {
        matches!(self, Self::Fabric | Self::Quilt)
    }
}

/// Per-server launch preferences that the player may override.
///
/// Deliberately carries no Java version: Mojang publishes `javaVersion` in every
/// release's `version.json`, including the oldest ones, so a manifest cannot add
/// anything but a wrong answer. A `java` block from an older manifest is accepted
/// and ignored, so existing manifests keep loading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaSpec {
    #[serde(default)]
    pub ram: Option<RamSpec>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RamSpec {
    pub recommended: u32,
    pub minimum: u32,
}

/// A mod to resolve from an upstream catalogue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModEntry {
    pub source: ModSource,
    /// Project slug or id. CurseForge uses numeric ids, so this accepts both.
    #[serde(deserialize_with = "string_or_number::deserialize")]
    pub id: String,
    /// Exact version id, or `latest`.
    #[serde(default)]
    pub version: Option<String>,
    /// CurseForge-only file id.
    #[serde(default)]
    pub file_id: Option<u64>,
    /// Required mods cannot be disabled in the UI.
    #[serde(default = "default_true")]
    pub required: bool,
    /// Disabled by default, even when not required.
    #[serde(default)]
    pub disabled_by_default: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModSource {
    Modrinth,
    Curseforge,
}

/// A file hosted elsewhere and placed inside the instance directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FileEntry {
    /// Relative path inside the instance directory.
    pub path: String,
    pub url: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub sha256: Option<String>,
    /// Modrinth publishes SHA-512 and SHA-1 rather than SHA-256, and a modpack
    /// archive carries exactly those, so all three are accepted.
    #[serde(default)]
    pub sha512: Option<String>,
    #[serde(default)]
    pub sha1: Option<String>,
    /// Replace an existing file even when the player modified it.
    #[serde(default)]
    pub overwrite: bool,
}

impl FileEntry {
    /// The strongest hash this entry carries, for verification.
    pub fn hash(&self) -> Option<crate::net::downloader::FileHash> {
        use crate::net::downloader::FileHash;

        if let Some(value) = self.sha512.clone() {
            return Some(FileHash::sha512(value));
        }
        if let Some(value) = self.sha256.clone() {
            return Some(FileHash::sha256(value));
        }
        self.sha1.clone().map(FileHash::sha1)
    }
}

fn default_true() -> bool {
    true
}

impl Manifest {
    pub fn parse(text: &str) -> Result<Self> {
        let manifest: Manifest = serde_json::from_str(text)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Reject documents this build cannot honour, with an actionable message.
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SUPPORTED_SCHEMA_VERSION {
            return Err(MangoError::Other(format!(
                "manifest schemaVersion {} is not supported (this build supports {})",
                self.schema_version, SUPPORTED_SCHEMA_VERSION
            )));
        }
        if self.servers.is_empty() {
            return Err(MangoError::Config("manifest declares no servers".to_owned()));
        }

        for server in &self.servers {
            if server.id.trim().is_empty() {
                return Err(MangoError::Config("a server has an empty id".to_owned()));
            }
            if server.minecraft.trim().is_empty() {
                return Err(MangoError::Config(format!(
                    "server `{}` has no minecraft version",
                    server.id
                )));
            }
            if server.loader.version.is_some() && server.loader.kind == LoaderKind::Vanilla {
                return Err(MangoError::Config(format!(
                    "server `{}` sets a loader version but loader.type is vanilla",
                    server.id
                )));
            }
            for file in &server.files {
                if file.path.starts_with('/') || file.path.contains("..") {
                    return Err(MangoError::Config(format!(
                        "server `{}` declares an unsafe file path `{}`",
                        server.id, file.path
                    )));
                }
            }
            for modification in &server.mods {
                if modification.id.trim().is_empty() {
                    return Err(MangoError::Config(format!(
                        "server `{}` has a mod with an empty id",
                        server.id
                    )));
                }
            }
        }

        if self.servers.iter().filter(|s| s.main_server).count() > 1 {
            return Err(MangoError::Config("more than one server is marked mainServer".to_owned()));
        }

        Ok(())
    }

    pub fn server(&self, id: &str) -> Option<&ServerEntry> {
        self.servers.iter().find(|server| server.id == id)
    }

    /// Server to select when the caller has no preference.
    ///
    /// Falls back to the first server, matching Helios' documented behaviour.
    pub fn default_server(&self) -> Option<&ServerEntry> {
        self.servers.iter().find(|server| server.main_server).or_else(|| self.servers.first())
    }

    /// Resolve a requested server id, falling back to the default.
    pub fn resolve_server(&self, id: Option<&str>) -> Result<&ServerEntry> {
        match id {
            Some(id) => self
                .server(id)
                .ok_or_else(|| MangoError::Other(format!("manifest has no server with id `{id}`"))),
            None => self
                .default_server()
                .ok_or_else(|| MangoError::Config("manifest declares no servers".to_owned())),
        }
    }
}

/// Accept a JSON string or number where an id is expected.
///
/// CurseForge project ids are numbers while Modrinth uses slugs, and forcing
/// manifest authors to quote them is an easy source of bugs.
mod string_or_number {
    use serde::{Deserialize, Deserializer};

    pub fn deserialize<'de, D>(deserializer: D) -> Result<String, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Value {
            String(String),
            Integer(i64),
        }

        Ok(match Value::deserialize(deserializer)? {
            Value::String(text) => text,
            Value::Integer(number) => number.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str =
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/manifest.json"));

    #[test]
    fn example_manifest_parses() {
        let manifest = Manifest::parse(SAMPLE).unwrap();
        assert_eq!(manifest.schema_version, 1);
        assert_eq!(manifest.name, "Mango Network");
        assert!(manifest.servers.len() >= 2);
    }

    #[test]
    fn a_manifest_that_still_pins_java_loads_and_the_override_is_ignored() {
        // Manifests written before the requirement moved to Mojang's metadata carry
        // a `java.supported`/`java.suggested` pair. Dropping the fields must not
        // make those manifests unreadable.
        let manifest = Manifest::parse(
            r#"{"schemaVersion":1,"name":"Legacy","servers":[
                {"id":"a","name":"A","address":"a.example.com","minecraft":"1.20.1",
                 "java":{"supported":">=16 <20","suggested":17,"distribution":"temurin",
                         "ram":{"recommended":4096,"minimum":2048}}}]}"#,
        )
        .unwrap();

        let java = manifest.servers[0].java.as_ref().expect("the block still parses");
        assert_eq!(java.ram.map(|ram| ram.recommended), Some(4096));
    }

    #[test]
    fn default_server_prefers_main_server() {
        let manifest = Manifest::parse(SAMPLE).unwrap();
        assert_eq!(manifest.default_server().unwrap().id, "main");
    }

    #[test]
    fn explicit_server_lookup_wins() {
        let manifest = Manifest::parse(SAMPLE).unwrap();
        assert_eq!(manifest.resolve_server(Some("creative")).unwrap().id, "creative");
        assert!(manifest.resolve_server(Some("nope")).is_err());
    }

    #[test]
    fn address_port_is_parsed() {
        let manifest = Manifest::parse(SAMPLE).unwrap();
        let server = manifest.server("main").unwrap();
        let (host, port) = server.host_port();
        assert_eq!(host, "play.example.com");
        assert_eq!(port, 25565);
    }

    #[test]
    fn loader_kind_defaults_to_vanilla() {
        let entry: ServerEntry =
            serde_json::from_str(r#"{"id":"a","name":"A","address":"h","minecraft":"1.20.1"}"#)
                .unwrap();
        assert_eq!(entry.loader.kind, LoaderKind::Vanilla);
        assert!(!entry.loader.kind.has_profile_json());
    }

    #[test]
    fn curseforge_numeric_ids_are_accepted() {
        let entry: ModEntry =
            serde_json::from_str(r#"{"source":"curseforge","id":238222,"fileId":4567890}"#)
                .unwrap();
        assert_eq!(entry.id, "238222");
        assert_eq!(entry.file_id, Some(4567890));
        assert!(entry.required);
    }

    #[test]
    fn modrinth_string_ids_are_accepted() {
        let entry: ModEntry =
            serde_json::from_str(r#"{"source":"modrinth","id":"sodium","required":false}"#)
                .unwrap();
        assert_eq!(entry.id, "sodium");
        assert!(!entry.required);
    }

    #[test]
    fn unsupported_schema_version_is_rejected() {
        let error = Manifest::parse(r#"{"schemaVersion":99,"name":"X","servers":[]}"#).unwrap_err();
        assert!(error.to_string().contains("not supported"));
    }

    #[test]
    fn unknown_fields_are_rejected() {
        let error =
            Manifest::parse(r#"{"schemaVersion":1,"name":"X","servers":[],"surprise":true}"#);
        assert!(error.is_err());
    }

    #[test]
    fn empty_server_list_is_rejected() {
        assert!(Manifest::parse(r#"{"schemaVersion":1,"name":"X","servers":[]}"#).is_err());
    }

    #[test]
    fn unsafe_file_paths_are_rejected() {
        let error = Manifest::parse(
            r#"{"schemaVersion":1,"name":"X","servers":[{
                "id":"a","name":"A","address":"h","minecraft":"1.20.1",
                "files":[{"path":"../../evil.txt","url":"https://example.com/x"}]}
            ]}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("unsafe file path"));
    }

    #[test]
    fn multiple_main_servers_are_rejected() {
        let error = Manifest::parse(
            r#"{"schemaVersion":1,"name":"X","servers":[
                {"id":"a","name":"A","address":"h","minecraft":"1.20.1","mainServer":true},
                {"id":"b","name":"B","address":"h","minecraft":"1.20.1","mainServer":true}
            ]}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("mainServer"));
    }

    #[test]
    fn vanilla_with_loader_version_is_rejected() {
        let error = Manifest::parse(
            r#"{"schemaVersion":1,"name":"X","servers":[
                {"id":"a","name":"A","address":"h","minecraft":"1.20.1",
                 "loader":{"type":"vanilla","version":"1.0"}}
            ]}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("loader"));
    }

    #[test]
    fn manifest_round_trips() {
        let manifest = Manifest::parse(SAMPLE).unwrap();
        let text = serde_json::to_string_pretty(&manifest).unwrap();
        let reparsed = Manifest::parse(&text).unwrap();
        assert_eq!(manifest, reparsed);
    }
}
