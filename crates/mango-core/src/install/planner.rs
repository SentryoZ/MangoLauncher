//! Building an [`InstallPlan`] from Mojang and loader metadata.
//!
//! Planning is pure: it takes parsed metadata and produces the full set of
//! files that must exist on disk. Nothing is downloaded or verified here, so a
//! plan can be compared against the local disk first and only the missing
//! pieces fetched.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::mojang::{AssetIndex, MavenCoordinate, RuleContext, VersionJson, version_json::Library};
use crate::net::downloader::{FileHash, FileKind, PlannedFile};

/// Mojang's asset CDN root.
pub const MOJANG_RESOURCES: &str = "https://resources.download.minecraft.net";
/// Maven repository used when a library omits an explicit URL.
pub const MOJANG_LIBRARIES: &str = "https://libraries.minecraft.net";

/// Where the plan's files live, derived from the launcher config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRoots {
    pub common_dir: PathBuf,
    pub instance_dir: PathBuf,
}

impl PlanRoots {
    pub fn new(common_dir: impl Into<PathBuf>, instance_dir: impl Into<PathBuf>) -> Self {
        Self { common_dir: common_dir.into(), instance_dir: instance_dir.into() }
    }

    pub fn assets_dir(&self) -> PathBuf {
        self.common_dir.join("assets")
    }

    pub fn libraries_dir(&self) -> PathBuf {
        self.common_dir.join("libraries")
    }

    pub fn versions_dir(&self) -> PathBuf {
        self.common_dir.join("versions")
    }

    pub fn client_jar(&self, version: &str) -> PathBuf {
        self.versions_dir().join(version).join(format!("{version}.jar"))
    }
}

/// A natives archive plus the entries to skip when extracting it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeArchive {
    pub archive: PathBuf,
    pub exclusions: Vec<String>,
}

/// The complete set of files a working installation needs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InstallPlan {
    pub files: Vec<PlannedFile>,
    /// Libraries added to the classpath, in load order.
    pub classpath: Vec<PathBuf>,
    /// Natives archives that must be extracted before launch.
    pub natives: Vec<NativeArchive>,
    pub main_class: Option<String>,
    /// Merged JVM arguments with placeholders still unresolved.
    pub jvm_arguments: Vec<String>,
    /// Merged game arguments with placeholders still unresolved.
    pub game_arguments: Vec<String>,
    pub base_version: String,
    pub java_major: Option<u32>,
}

impl InstallPlan {
    pub fn total_bytes(&self) -> u64 {
        self.files.iter().map(|file| file.size).sum()
    }

    pub fn kind_count(&self, kind: FileKind) -> usize {
        self.files.iter().filter(|file| file.kind == kind).count()
    }

    /// Absolute path of every file that must exist for this plan.
    pub fn paths(&self) -> impl Iterator<Item = &Path> {
        self.files.iter().map(|file| file.path.as_path())
    }
}

/// Build the install plan for a vanilla version.
pub fn plan_vanilla(
    version: &VersionJson,
    asset_index: &AssetIndex,
    roots: &PlanRoots,
    ctx: &RuleContext,
) -> InstallPlan {
    let mut plan = InstallPlan {
        base_version: version.id.clone(),
        main_class: version.main_class.clone(),
        jvm_arguments: version.jvm_arguments(ctx),
        game_arguments: version.game_arguments(ctx),
        java_major: version.java_version.as_ref().map(|jv| jv.major_version),
        ..Default::default()
    };

    if let Some(client) = version.downloads.as_ref().and_then(|d| d.client.as_ref()) {
        plan.files.push(PlannedFile {
            kind: FileKind::Client,
            path: roots.client_jar(&version.id),
            url: client.url.clone(),
            size: client.size,
            hash: client.sha1.clone().map(FileHash::sha1),
        });
    }

    // Version jar first, then libraries keyed by versionless maven id so a later
    // duplicate overrides the earlier entry (Helios relies on this for Forge).
    plan.classpath.push(roots.client_jar(&version.id));
    let mut libs: BTreeMap<String, PathBuf> = BTreeMap::new();
    for library in &version.libraries {
        if library.is_allowed(ctx) {
            plan_library(library, roots, ctx, &mut plan, &mut libs);
        }
    }
    plan.classpath.extend(libs.into_values());

    if let Some(asset_ref) = &version.asset_index {
        plan.files.push(PlannedFile {
            kind: FileKind::Asset,
            path: roots.assets_dir().join("indexes").join(format!("{}.json", asset_ref.id)),
            url: asset_ref.url.clone(),
            size: asset_ref.size,
            hash: asset_ref.sha1.clone().map(FileHash::sha1),
        });

        let assets_dir = roots.assets_dir();
        for object in asset_index.objects.values() {
            // Local layout keeps an `objects/` layer, but Mojang's CDN serves
            // objects at `/<first two hex chars>/<hash>`.
            let relative = AssetIndex::object_path(&object.hash);
            let prefix = &object.hash[..2.min(object.hash.len())];
            let remote = format!("{MOJANG_RESOURCES}/{prefix}/{}", object.hash);
            plan.files.push(PlannedFile {
                kind: FileKind::Asset,
                path: assets_dir.join(&relative),
                url: remote,
                size: object.size,
                hash: Some(FileHash::sha1(object.hash.clone())),
            });
        }
    }

    plan
}

/// Add the manifest's loose files to a plan.
///
/// Files marked `overwrite: false` are only queued when missing, so a config the
/// player edited survives reinstall. The check happens here rather than in the
/// downloader because "present" is meaningful without a hash.
pub fn add_manifest_files(
    plan: &mut InstallPlan,
    files: &[crate::manifest::FileEntry],
    instance_dir: &Path,
) {
    for entry in files {
        let path = instance_dir.join(entry.path.replace('/', std::path::MAIN_SEPARATOR_STR));
        if !entry.overwrite && path.exists() {
            continue;
        }
        plan.files.push(PlannedFile {
            kind: FileKind::Config,
            path,
            url: entry.url.clone(),
            size: entry.size,
            hash: entry.sha256.clone().map(FileHash::sha256),
        });
    }
}

fn plan_library(
    library: &Library,
    roots: &PlanRoots,
    ctx: &RuleContext,
    plan: &mut InstallPlan,
    classpath: &mut BTreeMap<String, PathBuf>,
) {
    let default_exclusions: &[&str] = &["META-INF/"];

    // Pre-1.19: a `natives` map selects a classifier per operating system.
    if let Some(natives) = &library.natives {
        let classifier =
            natives.get(ctx.os.as_str()).map(|raw| raw.replace("${arch}", ctx.arch.bitness()));
        if let Some(classifier) = classifier
            && let Some(artifact) = library
                .downloads
                .as_ref()
                .and_then(|d| d.classifiers.as_ref())
                .and_then(|c| c.get(&classifier))
        {
            add_native(library, artifact, roots, plan, default_exclusions);
        }
        return;
    }

    // 1.19+: natives are their own classifier libraries.
    //
    // Modern releases point the JVM at
    // `-Dorg.lwjgl.system.SharedLibraryExtractPath=${natives_directory}/lwjgl`,
    // which means LWJGL extracts the native libraries itself and must be able to
    // find the jars on the classpath. They are therefore recorded as natives (for
    // older profiles that expect a flat directory) *and* added to the classpath.
    if let Some(target) = library.native_target() {
        if target.os != ctx.os.as_str() {
            return;
        }
        if let Some(arch) = &target.arch {
            let matches = arch == ctx.arch.as_str()
                || (arch == "x64" && ctx.arch == crate::mojang::Arch::X86_64);
            if !matches {
                return;
            }
        }
        if let Some(artifact) = library.downloads.as_ref().and_then(|d| d.artifact.as_ref()) {
            let path = add_native(library, artifact, roots, plan, &["META-INF/", ".git", ".sha1"]);
            // Keyed by the *classifier-qualified* id: a natives jar must not
            // replace the real library, which shares its `group:artifact`.
            classpath.insert(format!("{}:natives", versionless(&library.name)), path);
        }
        return;
    }

    // Plain classpath library.
    let resolved = library
        .downloads
        .as_ref()
        .and_then(|d| d.artifact.as_ref())
        .map(|artifact| (artifact.url.clone(), artifact.size, artifact.sha1.clone(), None))
        .or_else(|| {
            // Fabric/Quilt style: `url` is the maven root and the hash sits at
            // the top level of the library entry.
            let coord = MavenCoordinate::parse(&library.name).ok()?;
            let base = library.url.as_deref().unwrap_or(MOJANG_LIBRARIES);
            let relative = coord.to_path();
            Some((
                format!("{}/{}", base.trim_end_matches('/'), relative),
                library.size,
                library.sha1.clone(),
                Some(relative),
            ))
        });

    let Some((url, size, sha1, relative_override)) = resolved else { return };
    let relative = relative_override.unwrap_or_else(|| maven_relative(&library.name));
    let path = roots.libraries_dir().join(&relative);

    // Prefer SHA-1 because that is what Mojang publishes; fall back to the
    // strongest hash the loader supplied.
    let hash = sha1
        .map(FileHash::sha1)
        .or_else(|| library.sha512.clone().map(FileHash::sha512))
        .or_else(|| library.sha256.clone().map(FileHash::sha256));

    plan.files.push(PlannedFile { kind: FileKind::Library, path: path.clone(), url, size, hash });

    // Server libraries override Mojang ones by versionless id.
    classpath.insert(versionless(&library.name), path);
}

fn add_native(
    library: &Library,
    artifact: &crate::mojang::version_json::Artifact,
    roots: &PlanRoots,
    plan: &mut InstallPlan,
    default_exclusions: &[&str],
) -> PathBuf {
    let relative =
        artifact.path.clone().unwrap_or_else(|| maven_relative_for(library, default_exclusions));
    let path = roots.libraries_dir().join(&relative);

    let exclusions = library
        .extract
        .as_ref()
        .map(|extract| extract.exclusions(default_exclusions))
        .unwrap_or_else(|| default_exclusions.iter().map(|s| (*s).to_owned()).collect());

    plan.files.push(PlannedFile {
        kind: FileKind::Native,
        path: path.clone(),
        url: artifact.url.clone(),
        size: artifact.size,
        hash: artifact.sha1.clone().map(FileHash::sha1),
    });
    plan.natives.push(NativeArchive { archive: path.clone(), exclusions });
    path
}

fn maven_relative(id: &str) -> String {
    MavenCoordinate::parse(id).map(|c| c.to_path()).unwrap_or_else(|_| id.replace(':', "/"))
}

fn maven_relative_for(library: &Library, _exclusions: &[&str]) -> String {
    maven_relative(&library.name)
}

fn versionless(id: &str) -> String {
    MavenCoordinate::parse(id).map(|c| c.versionless_id()).unwrap_or_else(|_| id.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mojang::rules::{Features, OsName};

    const VERSION: &str = r#"{
        "id": "1.20.1",
        "mainClass": "net.minecraft.client.main.Main",
        "assets": "5",
        "assetIndex": {"id": "5", "sha1": "aabb", "size": 100, "totalSize": 1000, "url": "https://example.com/index.json"},
        "downloads": {"client": {"sha1": "ccdd", "size": 500, "url": "https://example.com/client.jar"}},
        "javaVersion": {"majorVersion": 17},
        "libraries": [
            {"name": "com.example:plain:1.0",
             "downloads": {"artifact": {"path": "com/example/plain/1.0/plain-1.0.jar", "sha1": "11", "size": 10, "url": "https://example.com/plain.jar"}}},
            {"name": "com.example:native:1.0",
             "natives": {"windows": "natives-windows-${arch}", "linux": "natives-linux", "osx": "natives-osx"},
             "extract": {"exclude": ["META-INF/", "docs/"]},
             "downloads": {"classifiers": {"natives-windows-64": {"path": "com/example/native/1.0/native-w64.jar", "sha1": "22", "size": 20, "url": "https://example.com/native-w64.jar"},
                                            "natives-linux": {"path": "com/example/native/1.0/native-linux.jar", "sha1": "33", "size": 21, "url": "https://example.com/native-linux.jar"}}}},
            {"name": "com.example:modern-native:1.0:natives-windows",
             "downloads": {"artifact": {"path": "com/example/modern/1.0/modern-windows.jar", "sha1": "44", "size": 30, "url": "https://example.com/modern-windows.jar"}}},
            {"name": "com.example:modern-native:1.0:natives-linux",
             "downloads": {"artifact": {"path": "com/example/modern/1.0/modern-linux.jar", "sha1": "55", "size": 31, "url": "https://example.com/modern-linux.jar"}}},
            {"name": "com.example:mac:1.0",
             "rules": [{"action": "allow", "os": {"name": "osx"}}],
             "downloads": {"artifact": {"path": "com/example/mac/1.0/mac-1.0.jar", "sha1": "66", "size": 40, "url": "https://example.com/mac.jar"}}}
        ],
        "arguments": {"game": ["--username", "${auth_player_name}"], "jvm": ["-cp", "${classpath}"]}
    }"#;

    const ASSETS: &str = r#"{"objects": {"icons/icon.png": {"hash": "00112233aabb", "size": 7}}}"#;

    fn windows_ctx() -> RuleContext {
        RuleContext {
            os: OsName::Windows,
            arch: crate::mojang::Arch::X86_64,
            os_version: "10.0.22631".to_owned(),
            features: Features::default(),
        }
    }

    fn plan_for(ctx: &RuleContext) -> InstallPlan {
        let version = VersionJson::parse(VERSION.as_bytes()).unwrap();
        let assets = AssetIndex::parse(ASSETS.as_bytes()).unwrap();
        let roots = PlanRoots::new("/g/common", "/g/instances/main");
        plan_vanilla(&version, &assets, &roots, ctx)
    }

    #[test]
    fn plan_includes_client_libraries_and_assets() {
        let plan = plan_for(&windows_ctx());
        assert_eq!(plan.base_version, "1.20.1");
        assert_eq!(plan.java_major, Some(17));
        assert_eq!(plan.kind_count(FileKind::Client), 1);
        assert_eq!(plan.kind_count(FileKind::Library), 1);
        assert_eq!(plan.kind_count(FileKind::Native), 2);
        assert_eq!(plan.kind_count(FileKind::Asset), 2);
        assert_eq!(
            plan.files.iter().filter(|f| f.kind == FileKind::Asset).map(|f| f.size).sum::<u64>(),
            107
        );
    }

    #[test]
    fn windows_only_extracts_windows_natives() {
        let plan = plan_for(&windows_ctx());
        let archives: Vec<_> =
            plan.natives.iter().map(|n| n.archive.to_string_lossy().to_string()).collect();
        assert!(archives.iter().any(|a| a.contains("native-w64.jar")));
        assert!(archives.iter().any(|a| a.contains("modern-windows.jar")));
        assert!(!archives.iter().any(|a| a.contains("linux")));
    }

    #[test]
    fn native_extraction_exclusions_come_from_metadata() {
        let plan = plan_for(&windows_ctx());
        let legacy = plan
            .natives
            .iter()
            .find(|n| n.archive.to_string_lossy().contains("native-w64"))
            .unwrap();
        assert_eq!(legacy.exclusions, vec!["META-INF/", "docs/"]);

        let modern = plan
            .natives
            .iter()
            .find(|n| n.archive.to_string_lossy().contains("modern-windows"))
            .unwrap();
        assert_eq!(modern.exclusions, vec!["META-INF/", ".git", ".sha1"]);
    }

    #[test]
    fn os_rules_keep_mac_only_library_off_windows() {
        let plan = plan_for(&windows_ctx());
        assert!(!plan.files.iter().any(|f| f.path.to_string_lossy().contains("mac-1.0")));

        let mac_ctx = RuleContext { os: OsName::Osx, ..windows_ctx() };
        let mac_plan = plan_for(&mac_ctx);
        assert!(mac_plan.files.iter().any(|f| f.path.to_string_lossy().contains("mac-1.0")));
    }

    #[test]
    fn classpath_starts_with_client_jar() {
        let plan = plan_for(&windows_ctx());
        assert_eq!(plan.classpath[0], PathBuf::from("/g/common/versions/1.20.1/1.20.1.jar"));
        assert!(plan.classpath.iter().any(|p| p.ends_with("plain-1.0.jar")));
    }

    #[test]
    fn modern_natives_are_on_the_classpath_for_lwjgl_to_extract() {
        // Modern versions hand LWJGL a `SharedLibraryExtractPath`, so the jars
        // must be findable by the class loader.
        let plan = plan_for(&windows_ctx());
        assert!(
            plan.classpath.iter().any(|p| p.to_string_lossy().contains("modern-windows")),
            "natives jars must be on the classpath for modern versions"
        );
    }

    #[test]
    fn a_natives_jar_never_replaces_its_real_library() {
        // `com.mojang:jtracy:1.14.38` and its `:natives-windows` classifier share
        // a versionless id. Keying the classpath by that id made the natives jar
        // evict the real one, and the game failed with NoClassDefFoundError.
        let version: VersionJson = serde_json::from_str(
            r#"{
                "id": "26.3",
                "mainClass": "net.minecraft.client.main.Main",
                "libraries": [
                    {"name": "com.mojang:jtracy:1.14.38",
                     "downloads": {"artifact": {"path": "com/mojang/jtracy/1.14.38/jtracy-1.14.38.jar",
                                                "sha1": "aa", "size": 10,
                                                "url": "https://example.com/jtracy.jar"}}},
                    {"name": "com.mojang:jtracy:1.14.38:natives-windows",
                     "rules": [{"action": "allow", "os": {"name": "windows"}}],
                     "downloads": {"artifact": {"path": "com/mojang/jtracy/1.14.38/jtracy-1.14.38-natives-windows.jar",
                                                "sha1": "bb", "size": 20,
                                                "url": "https://example.com/jtracy-natives.jar"}}}
                ]
            }"#,
        )
        .unwrap();

        let assets = AssetIndex::parse(r#"{"objects":{}}"#.as_bytes()).unwrap();
        let plan =
            plan_vanilla(&version, &assets, &PlanRoots::new("/g/common", "/g/i"), &windows_ctx());

        let classpath: Vec<String> =
            plan.classpath.iter().map(|p| p.to_string_lossy().to_string()).collect();
        assert!(
            classpath.iter().any(|p| p.ends_with("jtracy-1.14.38.jar")),
            "the real library must stay: {classpath:?}"
        );
        assert!(
            classpath.iter().any(|p| p.ends_with("jtracy-1.14.38-natives-windows.jar")),
            "the natives jar must also be present: {classpath:?}"
        );
    }

    #[test]
    fn asset_objects_are_content_addressed() {
        let plan = plan_for(&windows_ctx());
        let object =
            plan.files.iter().find(|f| f.url.contains("resources.download.minecraft.net")).unwrap();
        // Local path keeps the objects/ layer, the CDN URL does not.
        assert_eq!(object.url, "https://resources.download.minecraft.net/00/00112233aabb");
        let local = object.path.to_string_lossy().replace('\\', "/");
        assert!(local.ends_with("assets/objects/00/00112233aabb"), "{local}");
        assert_eq!(object.hash.as_ref().unwrap().value, "00112233aabb");
    }

    #[test]
    fn arguments_are_carried_over_unresolved() {
        let plan = plan_for(&windows_ctx());
        assert_eq!(plan.main_class.as_deref(), Some("net.minecraft.client.main.Main"));
        assert!(plan.game_arguments.contains(&"${auth_player_name}".to_owned()));
        assert!(plan.jvm_arguments.contains(&"${classpath}".to_owned()));
    }
}
