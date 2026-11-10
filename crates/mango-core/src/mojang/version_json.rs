//! Deserialization of Mojang `version.json` documents.
//!
//! Works for both vanilla manifests and loader profiles (Fabric/Quilt), which
//! use the same shape.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::rules::{Rule, RuleContext, rules_allow};

/// A single downloadable file described by Mojang.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Artifact {
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: u64,
    pub url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LibraryDownloads {
    #[serde(default)]
    pub artifact: Option<Artifact>,
    #[serde(default)]
    pub classifiers: Option<BTreeMap<String, Artifact>>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extract {
    #[serde(default)]
    pub exclude: Vec<String>,
}

impl Extract {
    pub fn exclusions(&self, default: &[&str]) -> Vec<String> {
        if self.exclude.is_empty() {
            default.iter().map(|s| (*s).to_owned()).collect()
        } else {
            self.exclude.clone()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Library {
    pub name: String,
    #[serde(default)]
    pub downloads: Option<LibraryDownloads>,
    /// Pre-1.19 natives map: `{ "windows": "natives-windows-${arch}" }`.
    #[serde(default)]
    pub natives: Option<BTreeMap<String, String>>,
    #[serde(default)]
    pub rules: Vec<Rule>,
    #[serde(default)]
    pub extract: Option<Extract>,
    /// Maven repository root for libraries without explicit download URLs.
    ///
    /// Fabric and Quilt emit this shape — an `url` root plus a top-level hash —
    /// instead of Mojang's nested `downloads.artifact`.
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub sha512: Option<String>,
    #[serde(default)]
    pub size: u64,
}

impl Library {
    /// Whether this library applies on the current machine.
    pub fn is_allowed(&self, ctx: &RuleContext) -> bool {
        rules_allow(&self.rules, ctx)
    }

    /// Extract the `group:artifact` key used for classpath overriding.
    pub fn versionless_id(&self) -> Option<String> {
        super::maven::MavenCoordinate::parse(&self.name).ok().map(|coord| coord.versionless_id())
    }

    /// Detect a 1.19+ native classifier such as
    /// `org.lwjgl:lwjgl:3.3.1:natives-windows` or `...:natives-windows-arm64`.
    pub fn native_target(&self) -> Option<NativeTarget> {
        let coord = super::maven::MavenCoordinate::parse(&self.name).ok()?;
        let classifier = coord.classifier.as_deref()?;
        let rest = classifier.strip_prefix("natives-")?;
        let (os_part, arch) = match rest.rsplit_once('-') {
            Some((os, arch)) if matches!(arch, "arm64" | "x86" | "32" | "64") => {
                (os, Some(arch.to_owned()))
            }
            _ => (rest, None),
        };
        Some(NativeTarget { os: os_part.to_owned(), arch })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeTarget {
    pub os: String,
    pub arch: Option<String>,
}

/// One element of `arguments.jvm` / `arguments.game`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Argument {
    Plain(String),
    Conditional {
        #[serde(default)]
        rules: Vec<Rule>,
        value: ArgumentValue,
    },
}

impl Argument {
    pub fn is_allowed(&self, ctx: &RuleContext) -> bool {
        match self {
            Argument::Plain(_) => true,
            Argument::Conditional { rules, .. } => rules_allow(rules, ctx),
        }
    }

    pub fn values(&self) -> Vec<&str> {
        match self {
            Argument::Plain(value) => vec![value.as_str()],
            Argument::Conditional { value, .. } => match value {
                ArgumentValue::Single(value) => vec![value.as_str()],
                ArgumentValue::Many(values) => values.iter().map(String::as_str).collect(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ArgumentValue {
    Single(String),
    Many(Vec<String>),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Arguments {
    #[serde(default)]
    pub game: Vec<Argument>,
    #[serde(default)]
    pub jvm: Vec<Argument>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetIndexRef {
    pub id: String,
    #[serde(default)]
    pub sha1: Option<String>,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub total_size: u64,
    pub url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Downloads {
    #[serde(default)]
    pub client: Option<Artifact>,
    #[serde(default)]
    pub server: Option<Artifact>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JavaVersion {
    #[serde(default)]
    pub component: Option<String>,
    pub major_version: u32,
}

/// A parsed `version.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionJson {
    pub id: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub main_class: Option<String>,
    #[serde(default)]
    pub assets: Option<String>,
    #[serde(default)]
    pub asset_index: Option<AssetIndexRef>,
    #[serde(default)]
    pub libraries: Vec<Library>,
    #[serde(default)]
    pub arguments: Option<Arguments>,
    /// Pre-1.13 launch arguments.
    #[serde(default)]
    pub minecraft_arguments: Option<String>,
    #[serde(default)]
    pub downloads: Option<Downloads>,
    #[serde(default)]
    pub java_version: Option<JavaVersion>,
    /// Loader profiles reference a vanilla version to inherit from.
    #[serde(default)]
    pub inherits_from: Option<String>,
    #[serde(default)]
    pub release_time: Option<String>,
    #[serde(default)]
    pub time: Option<String>,
}

impl VersionJson {
    pub fn parse(bytes: &[u8]) -> crate::Result<Self> {
        Ok(serde_json::from_slice(bytes)?)
    }

    /// The version whose client jar this profile runs. Loader profiles such as
    /// Fabric set `inheritsFrom` to the vanilla version.
    pub fn base_version(&self) -> &str {
        self.inherits_from.as_deref().unwrap_or(&self.id)
    }

    /// Resolved game arguments, with rules evaluated and nested lists flattened.
    pub fn game_arguments(&self, ctx: &RuleContext) -> Vec<String> {
        let Some(arguments) = &self.arguments else { return Vec::new() };
        arguments
            .game
            .iter()
            .filter(|arg| arg.is_allowed(ctx))
            .flat_map(|arg| arg.values().into_iter().map(str::to_owned))
            .collect()
    }

    /// Resolved JVM arguments, with rules evaluated and nested lists flattened.
    pub fn jvm_arguments(&self, ctx: &RuleContext) -> Vec<String> {
        let Some(arguments) = &self.arguments else { return Vec::new() };
        arguments
            .jvm
            .iter()
            .filter(|arg| arg.is_allowed(ctx))
            .flat_map(|arg| arg.values().into_iter().map(str::to_owned))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "id": "1.20.1",
        "type": "release",
        "mainClass": "net.minecraft.client.main.Main",
        "assets": "5",
        "assetIndex": {
            "id": "5",
            "sha1": "abc",
            "size": 10,
            "totalSize": 20,
            "url": "https://example.com/5.json"
        },
        "downloads": {
            "client": {"sha1": "def", "size": 100, "url": "https://example.com/client.jar"}
        },
        "javaVersion": {"component": "java-runtime-gamma", "majorVersion": 17},
        "libraries": [
            {
                "name": "org.lwjgl:lwjgl:3.3.1",
                "downloads": {"artifact": {"path": "org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1.jar", "sha1": "111", "size": 5, "url": "https://example.com/lwjgl.jar"}}
            },
            {
                "name": "org.lwjgl:lwjgl:3.3.1:natives-windows",
                "downloads": {"artifact": {"path": "org/lwjgl/lwjgl/3.3.1/lwjgl-3.3.1-natives-windows.jar", "sha1": "222", "size": 5, "url": "https://example.com/lwjgl-natives.jar"}}
            },
            {
                "name": "com.example:mac-only:1.0",
                "rules": [{"action": "allow", "os": {"name": "osx"}}],
                "downloads": {"artifact": {"path": "x", "sha1": "333", "size": 1, "url": "https://example.com/mac.jar"}}
            }
        ],
        "arguments": {
            "game": [
                "--username", "${auth_player_name}",
                {"rules": [{"action": "allow", "features": {"has_custom_resolution": true}}], "value": ["--width", "${resolution_width}", "--height", "${resolution_height}"]},
                {"rules": [{"action": "allow", "features": {"is_demo_user": true}}], "value": ["--demo"]}
            ],
            "jvm": [
                "-Djava.library.path=${natives_directory}",
                "-cp", "${classpath}"
            ]
        }
    }"#;

    fn parse() -> VersionJson {
        serde_json::from_str(SAMPLE).unwrap()
    }

    #[test]
    fn parses_core_fields() {
        let version = parse();
        assert_eq!(version.id, "1.20.1");
        assert_eq!(version.main_class.as_deref(), Some("net.minecraft.client.main.Main"));
        assert_eq!(version.assets.as_deref(), Some("5"));
        assert_eq!(version.java_version.as_ref().unwrap().major_version, 17);
        assert_eq!(version.downloads.as_ref().unwrap().client.as_ref().unwrap().size, 100);
    }

    #[test]
    fn game_arguments_filter_by_rules() {
        let version = parse();
        let ctx = RuleContext::new();
        let args = version.game_arguments(&ctx);
        assert!(args.contains(&"--username".to_owned()));
        assert!(!args.contains(&"--demo".to_owned()));
        assert!(!args.contains(&"--width".to_owned()));
    }

    #[test]
    fn custom_resolution_args_are_flattened() {
        let version = parse();
        let ctx = RuleContext::new().with_features(super::super::rules::Features {
            has_custom_resolution: true,
            ..Default::default()
        });
        let args = version.game_arguments(&ctx);
        let width = args.iter().position(|a| a == "--width").unwrap();
        assert_eq!(args[width + 1], "${resolution_width}");
        let height = args.iter().position(|a| a == "--height").unwrap();
        assert_eq!(args[height + 1], "${resolution_height}");
    }

    #[test]
    fn library_rules_filter_by_os() {
        let version = parse();
        let ctx = RuleContext::new();
        let windows = &version.libraries[0];
        let mac_only = &version.libraries[2];
        assert_eq!(windows.is_allowed(&ctx), ctx.os.as_str() != "osx");
        assert_eq!(mac_only.is_allowed(&ctx), ctx.os.as_str() == "osx");
    }

    #[test]
    fn detects_modern_native_classifiers() {
        let version = parse();
        let target = version.libraries[1].native_target().unwrap();
        assert_eq!(target.os, "windows");
        assert_eq!(target.arch, None);
        assert!(version.libraries[0].native_target().is_none());
    }

    #[test]
    fn versionless_id_strips_version_and_classifier() {
        let version = parse();
        assert_eq!(version.libraries[1].versionless_id().as_deref(), Some("org.lwjgl:lwjgl"));
    }

    #[test]
    fn legacy_minecraft_arguments_are_kept() {
        let legacy: VersionJson = serde_json::from_str(
            r#"{"id":"1.7.10","minecraftArguments":"--username ${auth_player_name} --gameDir ${game_directory}"}"#,
        )
        .unwrap();
        assert!(legacy.arguments.is_none());
        assert!(legacy.minecraft_arguments.unwrap().contains("${game_directory}"));
    }

    #[test]
    fn inherits_from_wins_for_base_version() {
        let profile: VersionJson = serde_json::from_str(
            r#"{"id":"fabric-loader-0.16","inheritsFrom":"1.20.1","mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient"}"#,
        )
        .unwrap();
        assert_eq!(profile.base_version(), "1.20.1");
    }
}
