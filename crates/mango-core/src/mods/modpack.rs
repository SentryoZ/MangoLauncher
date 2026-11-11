//! Modrinth modpack (`.mrpack`) support.
//!
//! A modpack is a ZIP holding `modrinth.index.json` and an `overrides/` tree. The
//! index pins every mod to an exact file, with hashes and a download URL, so
//! installing a pack needs no version resolution: the pack author already decided.
//! `overrides/` carries the configs, resource packs, and anything else the pack
//! ships directly.
//!
//! That makes this much simpler than resolving loose mods: the index becomes a
//! list of [`FileEntry`]s, and the whole thing installs through the same plan and
//! lockfile path every other instance uses.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::Path;

use serde::Deserialize;

use crate::error::{MangoError, Result};
use crate::manifest::{FileEntry, LoaderKind, LoaderSpec};

/// Index file name inside the archive.
pub const INDEX_FILE: &str = "modrinth.index.json";

/// Directory holding files shipped inside the archive itself.
pub const OVERRIDES_DIR: &str = "overrides";

/// Format versions this build understands.
///
/// Only 1 exists; a newer value would mean fields whose meaning is unknown, so
/// guessing at them is worse than refusing.
const SUPPORTED_FORMAT_VERSION: u32 = 1;

/// The `modrinth.index.json` document.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModpackIndex {
    pub format_version: u32,
    /// `minecraft` for every pack in practice.
    #[serde(default)]
    pub game: String,
    pub version_id: String,
    pub name: String,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub files: Vec<IndexFile>,
    /// `minecraft` and a loader key such as `fabric-loader`, each with a version.
    #[serde(default)]
    pub dependencies: BTreeMap<String, String>,
}

/// One downloadable file in the pack.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IndexFile {
    /// Path relative to the instance directory, always `/`-separated.
    pub path: String,
    #[serde(default)]
    pub hashes: IndexHashes,
    #[serde(default)]
    pub downloads: Vec<String>,
    #[serde(default)]
    pub file_size: u64,
    /// Which sides need the file. Absent means both.
    #[serde(default)]
    pub env: Option<FileEnv>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct IndexHashes {
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub sha512: Option<String>,
}

/// Side restrictions for a file.
#[derive(Debug, Clone, Deserialize)]
pub struct FileEnv {
    #[serde(default)]
    pub client: Option<String>,
    #[serde(default)]
    pub server: Option<String>,
}

impl FileEnv {
    /// Whether a client install needs this file.
    ///
    /// `unsupported` means the file belongs to the other side only. A pack that
    /// lists a server-only jar would otherwise be installed into the client, which
    /// is harmless in the mods folder but wasteful and confusing.
    fn wanted_on_client(&self) -> bool {
        !matches!(self.client.as_deref(), Some("unsupported"))
    }
}

impl ModpackIndex {
    /// Parse and validate an index document.
    pub fn parse(text: &str) -> Result<Self> {
        let index: Self = serde_json::from_str(crate::config::strip_bom(text))?;
        index.validate()?;
        Ok(index)
    }

    fn validate(&self) -> Result<()> {
        if self.format_version != SUPPORTED_FORMAT_VERSION {
            return Err(MangoError::Other(format!(
                "modpack format version {} is not supported (this build supports \
                 {SUPPORTED_FORMAT_VERSION})",
                self.format_version
            )));
        }

        if !self.game.is_empty() && self.game != "minecraft" {
            return Err(MangoError::Other(format!(
                "this pack targets `{}`, not Minecraft",
                self.game
            )));
        }

        // The Minecraft version decides which Java the pack needs and which loader
        // builds exist, so a pack without it cannot be installed at all.
        if self.minecraft().is_none() {
            return Err(MangoError::Other(
                "the pack does not name a Minecraft version in its dependencies".to_owned(),
            ));
        }

        for file in &self.files {
            if file.path.trim().is_empty() {
                return Err(MangoError::Other("the pack lists a file with no path".to_owned()));
            }
            if !is_safe_relative(&file.path) {
                return Err(MangoError::Other(format!(
                    "the pack lists an unsafe path `{}`",
                    file.path
                )));
            }
            if file.downloads.iter().all(|url| url.trim().is_empty()) {
                return Err(MangoError::Other(format!(
                    "the pack file `{}` has no download URL",
                    file.path
                )));
            }
        }

        Ok(())
    }

    /// Minecraft version the pack targets.
    pub fn minecraft(&self) -> Option<&str> {
        self.dependencies.get("minecraft").map(String::as_str)
    }

    /// Loader and version the pack targets.
    ///
    /// Modrinth keys are `<loader>-loader`, e.g. `fabric-loader`. Forge and
    /// NeoForge use the same shape in practice, so the suffix is stripped rather
    /// than matched against a fixed list.
    pub fn loader(&self) -> LoaderSpec {
        for (key, version) in &self.dependencies {
            if key == "minecraft" {
                continue;
            }

            let Some(name) = key.strip_suffix("-loader") else { continue };

            let Some(kind) = loader_from_name(name) else { continue };

            return LoaderSpec { kind, version: Some(version.clone()) };
        }

        LoaderSpec::default()
    }

    /// The files a client install needs, as manifest file entries.
    ///
    /// Server-only files are dropped, and the first download URL is used: packs
    /// list CDN mirrors, and any one of them serves the same bytes.
    pub fn client_files(&self) -> Vec<FileEntry> {
        self.files
            .iter()
            .filter(|file| file.env.as_ref().is_none_or(FileEnv::wanted_on_client))
            .filter_map(|file| {
                let url = file.downloads.iter().find(|url| !url.trim().is_empty())?;
                Some(FileEntry {
                    path: file.path.clone(),
                    url: url.clone(),
                    size: file.file_size,
                    sha256: None,
                    sha512: file.hashes.sha512.clone(),
                    sha1: file.hashes.sha1.clone(),
                    // A pack update must replace the old jar, so the pack owns these
                    // paths outright. Player edits belong in `overrides/`.
                    overwrite: true,
                })
            })
            .collect()
    }
}

/// Map a Modrinth loader name to our enum.
fn loader_from_name(name: &str) -> Option<LoaderKind> {
    match name {
        "fabric" => Some(LoaderKind::Fabric),
        "quilt" => Some(LoaderKind::Quilt),
        "forge" => Some(LoaderKind::Forge),
        "neoforge" => Some(LoaderKind::Neoforge),
        _ => None,
    }
}

/// Whether a pack path stays inside the instance directory.
///
/// The index comes from a downloaded archive, so an absolute path or a `..`
/// component would let a pack write anywhere on disk.
fn is_safe_relative(path: &str) -> bool {
    if path.starts_with('/') || path.contains('\\') || path.contains(':') {
        return false;
    }

    !path.split('/').any(|component| component == ".." || component == ".")
}

/// A modpack archive that has been read into memory.
#[derive(Debug)]
pub struct Modpack {
    pub index: ModpackIndex,
    /// `overrides/` entries as `(path inside overrides, file contents)`.
    ///
    /// These are files the pack ships directly rather than linking. They are small
    /// (configs, resource packs), which is why holding them is acceptable.
    pub overrides: Vec<(String, Vec<u8>)>,
}

impl Modpack {
    /// Read an archive from disk.
    pub fn open(path: &Path) -> Result<Self> {
        let file = std::fs::File::open(path)
            .map_err(|err| MangoError::Other(format!("could not open the pack: {err}")))?;
        Self::read(file)
    }

    /// Read an archive from any reader.
    pub fn read<R: Read + std::io::Seek>(reader: R) -> Result<Self> {
        let mut archive = zip::ZipArchive::new(reader)
            .map_err(|err| MangoError::Other(format!("the pack is not a valid ZIP: {err}")))?;

        let mut index_text = None;
        let mut overrides = Vec::new();

        for position in 0..archive.len() {
            let mut entry = archive.by_index(position).map_err(|err| {
                MangoError::Other(format!("could not read an entry in the pack: {err}"))
            })?;

            // `enclosed_name` rejects entries that escape the archive root, which a
            // malicious pack could otherwise use to write outside the instance.
            let Some(entry_path) = entry.enclosed_name() else { continue };
            let name = entry_path.to_string_lossy().replace('\\', "/");

            if name == INDEX_FILE {
                let mut text = String::new();
                entry.read_to_string(&mut text).map_err(|err| {
                    MangoError::Other(format!("the pack index is not readable text: {err}"))
                })?;
                index_text = Some(text);
                continue;
            }

            let Some(relative) = name.strip_prefix(&format!("{OVERRIDES_DIR}/")) else { continue };
            if relative.is_empty() || entry.is_dir() {
                continue;
            }

            // The override path is used as a filename later, so it gets the same
            // scrutiny as an index path.
            if !is_safe_relative(relative) {
                return Err(MangoError::Other(format!(
                    "the pack ships an unsafe override path `{relative}`"
                )));
            }

            let mut contents = Vec::new();
            entry.read_to_end(&mut contents).map_err(|err| {
                MangoError::Other(format!("could not read the pack override `{relative}`: {err}"))
            })?;
            overrides.push((relative.to_owned(), contents));
        }

        let Some(index_text) = index_text else {
            return Err(MangoError::Other(format!(
                "the archive has no {INDEX_FILE}; is it a Modrinth pack?"
            )));
        };

        Ok(Self { index: ModpackIndex::parse(&index_text)?, overrides })
    }

    /// Extract the `overrides/` tree into an instance directory.
    ///
    /// Existing files are left alone: those are the player's, and a pack update
    /// must not silently discard the config they tuned.
    pub fn extract_overrides(&self, instance_dir: &Path) -> Result<usize> {
        let mut written = 0;

        for (relative, contents) in &self.overrides {
            let target = instance_dir.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR));
            if target.exists() {
                continue;
            }

            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, contents)?;
            written += 1;
        }

        Ok(written)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = r#"{
        "formatVersion": 1,
        "game": "minecraft",
        "versionId": "5.2.8",
        "name": "Fabulously Optimized",
        "summary": "Performance",
        "dependencies": { "minecraft": "1.20.1", "fabric-loader": "0.14.22" },
        "files": [
            { "path": "mods/sodium.jar",
              "hashes": { "sha1": "aa", "sha512": "bb" },
              "downloads": ["https://cdn.modrinth.com/a/sodium.jar"],
              "fileSize": 971552 },
            { "path": "mods/server-only.jar",
              "hashes": { "sha512": "cc" },
              "downloads": ["https://cdn.modrinth.com/a/server.jar"],
              "fileSize": 10,
              "env": { "client": "unsupported", "server": "required" } }
        ]
    }"#;

    #[test]
    fn the_metadata_of_a_real_pack_parses() {
        let index = ModpackIndex::parse(INDEX).unwrap();
        assert_eq!(index.name, "Fabulously Optimized");
        assert_eq!(index.version_id, "5.2.8");
        assert_eq!(index.minecraft(), Some("1.20.1"));
    }

    #[test]
    fn the_loader_comes_from_the_dependencies_block() {
        let index = ModpackIndex::parse(INDEX).unwrap();
        let loader = index.loader();
        assert_eq!(loader.kind, LoaderKind::Fabric);
        assert_eq!(loader.version.as_deref(), Some("0.14.22"));
    }

    #[test]
    fn a_pack_without_a_loader_installs_as_vanilla() {
        let index = ModpackIndex::parse(
            r#"{"formatVersion":1,"versionId":"1","name":"Plain",
                "dependencies":{"minecraft":"1.20.1"},"files":[]}"#,
        )
        .unwrap();
        assert_eq!(index.loader().kind, LoaderKind::Vanilla);
        assert!(index.loader().version.is_none());
    }

    #[test]
    fn forge_and_neoforge_keys_are_recognised() {
        for (key, expected) in
            [("forge-loader", LoaderKind::Forge), ("neoforge-loader", LoaderKind::Neoforge)]
        {
            let index = ModpackIndex::parse(&format!(
                r#"{{"formatVersion":1,"versionId":"1","name":"P",
                    "dependencies":{{"minecraft":"1.20.1","{key}":"47.1.0"}},"files":[]}}"#
            ))
            .unwrap();
            assert_eq!(index.loader().kind, expected, "{key}");
        }
    }

    #[test]
    fn files_become_entries_with_the_hashes_the_pack_publishes() {
        let index = ModpackIndex::parse(INDEX).unwrap();
        let files = index.client_files();

        assert_eq!(files.len(), 1, "the server-only jar must be dropped");
        let sodium = &files[0];
        assert_eq!(sodium.path, "mods/sodium.jar");
        assert_eq!(sodium.url, "https://cdn.modrinth.com/a/sodium.jar");
        assert_eq!(sodium.size, 971552);
        assert_eq!(sodium.sha512.as_deref(), Some("bb"));
        assert_eq!(sodium.sha1.as_deref(), Some("aa"));
        assert!(sodium.overwrite, "the pack owns its own files");
    }

    #[test]
    fn a_server_only_file_is_not_installed_on_the_client() {
        let index = ModpackIndex::parse(INDEX).unwrap();
        assert!(
            index.client_files().iter().all(|file| !file.path.contains("server-only")),
            "a server-only jar would be dead weight in the client"
        );
    }

    #[test]
    fn a_file_without_an_env_block_is_needed_by_both_sides() {
        let env: FileEnv = serde_json::from_str(r#"{}"#).unwrap();
        assert!(env.wanted_on_client());
    }

    #[test]
    fn an_unsupported_format_version_is_refused() {
        let error = ModpackIndex::parse(
            r#"{"formatVersion":2,"versionId":"1","name":"P",
                "dependencies":{"minecraft":"1.20.1"},"files":[]}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("not supported"), "{error}");
    }

    #[test]
    fn a_pack_for_another_game_is_refused() {
        let error = ModpackIndex::parse(
            r#"{"formatVersion":1,"game":"hytale","versionId":"1","name":"P",
                "dependencies":{"minecraft":"1.20.1"},"files":[]}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("not Minecraft"), "{error}");
    }

    #[test]
    fn a_pack_without_a_minecraft_version_is_refused() {
        // Without it the launcher cannot pick Java or a loader build.
        let error = ModpackIndex::parse(
            r#"{"formatVersion":1,"versionId":"1","name":"P","dependencies":{},"files":[]}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("Minecraft version"), "{error}");
    }

    #[test]
    fn a_path_that_escapes_the_instance_is_refused() {
        for path in ["../evil.jar", "/etc/passwd", "mods/../../evil.jar", "C:/evil.jar"] {
            let document = format!(
                r#"{{"formatVersion":1,"versionId":"1","name":"P",
                    "dependencies":{{"minecraft":"1.20.1"}},
                    "files":[{{"path":"{path}","downloads":["https://e/x.jar"]}}]}}"#
            );
            let error = ModpackIndex::parse(&document).unwrap_err();
            assert!(error.to_string().contains("unsafe"), "{path} was accepted: {error}");
        }
    }

    #[test]
    fn a_file_with_no_download_url_is_refused() {
        let error = ModpackIndex::parse(
            r#"{"formatVersion":1,"versionId":"1","name":"P",
                "dependencies":{"minecraft":"1.20.1"},
                "files":[{"path":"mods/a.jar","downloads":[]}]}"#,
        )
        .unwrap_err();
        assert!(error.to_string().contains("no download URL"), "{error}");
    }

    #[test]
    fn an_archive_without_an_index_is_refused() {
        // A plain ZIP or a CurseForge pack: the message has to say which format was
        // expected, or the player has no idea what went wrong.
        let mut buffer = std::io::Cursor::new(Vec::new());
        {
            use std::io::Write;
            let mut writer = zip::ZipWriter::new(&mut buffer);
            writer.start_file("readme.txt", zip::write::SimpleFileOptions::default()).unwrap();
            writer.write_all(b"hi").unwrap();
            writer.finish().unwrap();
        }
        buffer.set_position(0);

        let error = Modpack::read(buffer).unwrap_err();
        assert!(error.to_string().contains("modrinth.index.json"), "{error}");
    }

    #[test]
    fn a_real_archive_yields_its_index_and_overrides() {
        use std::io::Write;

        let mut buffer = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            let options = zip::write::SimpleFileOptions::default();

            writer.start_file(INDEX_FILE, options).unwrap();
            writer.write_all(INDEX.as_bytes()).unwrap();

            writer.start_file("overrides/config/sodium.json", options).unwrap();
            writer.write_all(br#"{"quality":1}"#).unwrap();

            writer.start_file("overrides/options.txt", options).unwrap();
            writer.write_all(b"fov:70").unwrap();

            writer.finish().unwrap();
        }
        buffer.set_position(0);

        let pack = Modpack::read(buffer).unwrap();
        assert_eq!(pack.index.name, "Fabulously Optimized");
        assert_eq!(pack.overrides.len(), 2);

        let paths: Vec<&str> = pack.overrides.iter().map(|(path, _)| path.as_str()).collect();
        assert!(paths.contains(&"config/sodium.json"), "{paths:?}");
        assert!(paths.contains(&"options.txt"), "{paths:?}");
    }

    #[test]
    fn overrides_land_in_the_instance_and_keep_existing_files() {
        let dir = std::env::temp_dir().join(format!("mango-mrpack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("config")).unwrap();

        let pack = Modpack {
            index: ModpackIndex::parse(INDEX).unwrap(),
            overrides: vec![
                ("config/sodium.json".to_owned(), br#"{"quality":1}"#.to_vec()),
                ("options.txt".to_owned(), b"fov:70".to_vec()),
            ],
        };

        // The player already tuned this one, so the pack must not replace it.
        std::fs::write(dir.join("config/sodium.json"), b"player-edited").unwrap();

        let written = pack.extract_overrides(&dir).unwrap();
        assert_eq!(written, 1, "only the new file is written");

        assert_eq!(std::fs::read(dir.join("config/sodium.json")).unwrap(), b"player-edited");
        assert_eq!(std::fs::read(dir.join("options.txt")).unwrap(), b"fov:70");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_nested_override_directory_is_created() {
        let dir = std::env::temp_dir().join(format!("mango-mrpack-nest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);

        let pack = Modpack {
            index: ModpackIndex::parse(INDEX).unwrap(),
            overrides: vec![("config/deep/nested/file.json".to_owned(), b"{}".to_vec())],
        };

        pack.extract_overrides(&dir).unwrap();
        assert!(dir.join("config/deep/nested/file.json").is_file());

        std::fs::remove_dir_all(&dir).ok();
    }
}
