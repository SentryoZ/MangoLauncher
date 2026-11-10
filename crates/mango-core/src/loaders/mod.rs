//! Resolving and installing mod loaders.
//!
//! Two families exist:
//!
//! * **Profile loaders** (Fabric, Quilt) publish a complete `version.json` for
//!   every game/loader pair, so installing is "download the profile, inherit
//!   vanilla".
//! * **Installer loaders** (Forge, NeoForge) ship a jar that must be executed to
//!   patch the client and generate the profile. Reimplementing their processor
//!   chain is fragile, so the official installer is run headlessly.

pub mod fabric;
pub mod forge;
pub mod quilt;
pub mod versions;

pub use versions::{available as available_versions, supports as supports_version};

use crate::error::Result;
use crate::mojang::VersionJson;
use crate::mojang::rules::RuleContext;

/// A resolved loader profile, ready to merge with the vanilla version.
#[derive(Debug, Clone, PartialEq)]
pub struct LoaderProfile {
    /// `version.json` of the loader, with `inheritsFrom` set to the game version.
    pub profile: VersionJson,
    /// Version id the game should be launched as, e.g. `fabric-loader-0.16.9-1.20.1`.
    #[allow(dead_code)]
    pub version_id: String,
    /// Extra JVM arguments the loader requires.
    pub jvm_args: Vec<String>,
    /// Extra game arguments the loader requires.
    pub game_args: Vec<String>,
}

/// Merge a loader profile into the vanilla version.
///
/// Loader profiles only add libraries and arguments; the vanilla data stays
/// authoritative for main class discovery and asset selection unless the loader
/// overrides it — which Forge does, since its main class replaces vanilla's.
pub fn merge(base: &VersionJson, loader: &VersionJson, ctx: &RuleContext) -> VersionJson {
    let mut merged = base.clone();

    if let Some(main_class) = &loader.main_class {
        merged.main_class = Some(main_class.clone());
    }
    if loader.assets.is_some() {
        merged.assets = loader.assets.clone();
    }

    // Loader libraries come after Mojang's so the classpath builder can prefer
    // them by versionless maven id.
    merged.libraries.extend(loader.libraries.iter().cloned());

    // Arguments merge rather than replace: Fabric contributes JVM flags while
    // the game arguments still come from vanilla.
    let loader_jvm = loader.jvm_arguments(ctx);
    let loader_game = loader.game_arguments(ctx);

    match (&mut merged.arguments, loader.arguments.is_some()) {
        (Some(existing), true) => {
            if let Some(loader_arguments) = &loader.arguments {
                existing.jvm.extend(loader_arguments.jvm.iter().cloned());
                existing.game.extend(loader_arguments.game.iter().cloned());
            }
        }
        (None, true) if !loader_jvm.is_empty() || !loader_game.is_empty() => {
            merged.arguments = loader.arguments.clone();
        }
        _ => {}
    }

    merged
}

/// Whether a loader profile declares its own main class.
pub fn overrides_main_class(loader: &VersionJson) -> bool {
    loader.main_class.is_some() && loader.id != "vanilla"
}

/// Load the loader profile for a game version.
pub async fn resolve(
    client: &reqwest::Client,
    common_dir: &std::path::Path,
    kind: crate::manifest::LoaderKind,
    minecraft: &str,
    version: Option<&str>,
) -> Result<LoaderProfile> {
    use crate::manifest::LoaderKind;

    match kind {
        LoaderKind::Vanilla => Ok(LoaderProfile {
            profile: VersionJson {
                id: minecraft.to_owned(),
                kind: "release".to_owned(),
                main_class: None,
                assets: None,
                asset_index: None,
                libraries: Vec::new(),
                arguments: None,
                minecraft_arguments: None,
                downloads: None,
                java_version: None,
                inherits_from: None,
                release_time: None,
                time: None,
            },
            version_id: minecraft.to_owned(),
            jvm_args: Vec::new(),
            game_args: Vec::new(),
        }),
        LoaderKind::Fabric => fabric::resolve(client, minecraft, version).await,
        LoaderKind::Quilt => quilt::resolve(client, minecraft, version).await,
        LoaderKind::Forge => forge::resolve_forge(client, common_dir, minecraft, version).await,
        LoaderKind::Neoforge => {
            forge::resolve_neoforge(client, common_dir, minecraft, version).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mojang::rules::{Features, OsName};

    fn ctx() -> RuleContext {
        RuleContext {
            os: OsName::Windows,
            arch: crate::mojang::Arch::X86_64,
            os_version: "10.0.22631".to_owned(),
            features: Features::default(),
        }
    }

    fn vanilla() -> VersionJson {
        serde_json::from_str(
            r#"{"id":"1.20.1","mainClass":"net.minecraft.client.main.Main",
                "libraries":[{"name":"com.example:vanilla:1.0"}],
                "arguments":{"game":["--username","${auth_player_name}"],"jvm":["-cp","${classpath}"]}}"#,
        )
        .unwrap()
    }

    fn loader() -> VersionJson {
        serde_json::from_str(
            r#"{"id":"fabric-loader-0.16.9-1.20.1","inheritsFrom":"1.20.1",
                "mainClass":"net.fabricmc.loader.impl.launch.knot.KnotClient",
                "libraries":[{"name":"net.fabricmc:fabric-loader:0.16.9"}],
                "arguments":{"game":["--fabric.something"],"jvm":["-DFabricMcEmu=net.minecraft.client.main.Main"]}}"#,
        )
        .unwrap()
    }

    #[test]
    fn merge_overrides_main_class_and_appends_libraries() {
        let merged = merge(&vanilla(), &loader(), &ctx());
        assert_eq!(
            merged.main_class.as_deref(),
            Some("net.fabricmc.loader.impl.launch.knot.KnotClient")
        );
        assert_eq!(merged.libraries.len(), 2);
        assert_eq!(merged.libraries[1].name, "net.fabricmc:fabric-loader:0.16.9");
    }

    #[test]
    fn merge_keeps_vanilla_arguments_and_adds_loader_ones() {
        let merged = merge(&vanilla(), &loader(), &ctx());
        let game = merged.arguments.as_ref().unwrap();
        assert_eq!(game.game.len(), 3);
        assert_eq!(game.jvm.len(), 3);

        let rendered = merged.game_arguments(&ctx());
        assert!(rendered.contains(&"--fabric.something".to_owned()));
    }

    #[test]
    fn merge_keeps_vanilla_main_class_when_loader_has_none() {
        let mut loader = loader();
        loader.main_class = None;
        let merged = merge(&vanilla(), &loader, &ctx());
        assert_eq!(merged.main_class.as_deref(), Some("net.minecraft.client.main.Main"));
    }

    #[test]
    fn merge_adds_arguments_when_vanilla_has_none() {
        let mut base = vanilla();
        base.arguments = None;
        let merged = merge(&base, &loader(), &ctx());
        assert!(merged.arguments.is_some());
    }

    #[test]
    fn overrides_main_class_ignores_components_without_one() {
        let mut loader = loader();
        assert!(overrides_main_class(&loader));
        loader.main_class = None;
        assert!(!overrides_main_class(&loader));
    }
}
