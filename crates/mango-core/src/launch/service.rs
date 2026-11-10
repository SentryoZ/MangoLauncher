//! End-to-end launch flow: resolve metadata, plan arguments, extract natives,
//! and start the JVM.

use std::path::{Path, PathBuf};

use reqwest::Client;
use tracing::info;

use crate::error::{MangoError, Result};
use crate::install::natives::{
    extract_natives, new_natives_dir, prepare_native_dirs, purge_stale_natives,
};
use crate::install::planner::{InstallPlan, PlanRoots};
use crate::install::service::{self, ResolvedVersion};
use crate::instance::{InstanceOrigin, InstanceSpec};
use crate::launch::args::{LaunchContext, LoaderArguments, build_arguments, classpath_separator};
use crate::launch::classpath::{self, ClasspathEntry, LoaderClasspath};
use crate::launch::process::{self, GameProcess, LaunchRequest, natives_root};
use crate::manifest::{LoaderKind, LoaderSpec};
use crate::mojang::rules::RuleContext;

/// Account details needed to authenticate the game session.
#[derive(Debug, Clone)]
pub struct LaunchAccount {
    pub username: String,
    pub uuid: String,
    pub access_token: String,
    /// `msa` for Microsoft, `mojang` for Yggdrasil.
    pub user_type: String,
}

impl LaunchAccount {
    /// Offline profile used for testing and for offline play.
    pub fn offline(username: &str) -> Self {
        Self {
            username: username.to_owned(),
            uuid: offline_uuid(username),
            access_token: "0".to_owned(),
            user_type: "legacy".to_owned(),
        }
    }

    /// A signed-in Microsoft account, ready to launch.
    ///
    /// `user_type` must be `msa`: the game sends it to the session server, and
    /// using `mojang` for a Microsoft token is rejected.
    pub fn microsoft(meta: &crate::config::AccountMeta, access_token: impl Into<String>) -> Self {
        Self {
            username: meta.username.clone(),
            uuid: meta.uuid.clone(),
            access_token: access_token.into(),
            user_type: "msa".to_owned(),
        }
    }

    /// A signed-in Yggdrasil account (including custom authlib servers).
    pub fn yggdrasil(meta: &crate::config::AccountMeta, access_token: impl Into<String>) -> Self {
        Self {
            username: meta.username.clone(),
            uuid: meta.uuid.clone(),
            access_token: access_token.into(),
            user_type: "mojang".to_owned(),
        }
    }

    /// Whether this profile is a real signed-in account.
    ///
    /// Online play needs a token from Mojang; offline profiles use the sentinel
    /// `"0"` and are rejected by the session server.
    pub fn is_online(&self) -> bool {
        self.access_token != "0" && !self.access_token.trim().is_empty()
    }
}

/// Deterministic UUID for offline profiles, like the vanilla launcher's
/// name-based UUID.
pub fn offline_uuid(username: &str) -> String {
    let digest =
        crate::net::hash::hash_bytes(crate::net::hash::HashAlgorithm::Md5, username.as_bytes());
    // Set the version/variant nibbles so the value is a well-formed UUID.
    let mut chars: Vec<char> = digest.chars().collect();
    chars[12] = '3';
    chars[16] = '8';
    let uuid: String = chars.into_iter().collect();
    format!(
        "{}-{}-{}-{}-{}",
        &uuid[0..8],
        &uuid[8..12],
        &uuid[12..16],
        &uuid[16..20],
        &uuid[20..32]
    )
}

/// What to launch.
#[derive(Debug, Clone)]
pub struct LaunchSpec {
    /// Vanilla version id, e.g. `1.20.1`.
    pub minecraft_version: String,
    /// Profile id shown to the game as `${version_name}` (usually the server id).
    pub version_name: String,
    pub game_directory: PathBuf,
    pub java_executable: PathBuf,
    pub account: LaunchAccount,
    /// Mod loader declared by the server.
    pub loader: LoaderSpec,
    pub detached: bool,
    pub resolution: Option<(u32, u32)>,
    pub fullscreen: bool,
    /// Extra JVM flags from settings (RAM, GC, user options).
    pub jvm_options: Vec<String>,
    /// Optional `-javaagent` arguments (authlib-injector).
    pub java_agents: Vec<String>,
    /// Address to auto-join, `host:port`.
    pub auto_connect: Option<String>,
}

impl LaunchSpec {
    /// Classpath policy implied by the loader.
    pub fn classpath_policy(&self) -> LoaderClasspath {
        match self.loader.kind {
            LoaderKind::Vanilla => LoaderClasspath::Vanilla,
            LoaderKind::Fabric => LoaderClasspath::Fabric,
            LoaderKind::Quilt => LoaderClasspath::Quilt,
            LoaderKind::Forge => LoaderClasspath::Forge,
            LoaderKind::Neoforge => LoaderClasspath::NeoForge,
        }
    }
}

/// Prepare everything a launch needs, without starting the process.
#[derive(Debug, Clone)]
pub struct PreparedLaunch {
    pub arguments: Vec<String>,
    pub natives_dir: PathBuf,
    pub plan: InstallPlan,
}

/// Load the vanilla version and build its install plan.
pub async fn prepare(client: &Client, spec: &LaunchSpec) -> Result<ResolvedVersion> {
    service::resolve(client, &spec.minecraft_version).await
}

/// Build the argument vector and extract natives.
pub async fn build(
    client: &Client,
    spec: &LaunchSpec,
    roots: &PlanRoots,
    rule_ctx: &RuleContext,
    temp_dir: &Path,
) -> Result<PreparedLaunch> {
    // Paths passed to the JVM must be absolute: the process runs with the game
    // directory as its working directory, so relative paths would resolve
    // against the wrong base.
    let roots = PlanRoots::new(absolute(&roots.common_dir), absolute(&roots.instance_dir));
    let roots = &roots;
    let game_directory = absolute(&spec.game_directory);

    let resolved = service::resolve(client, &spec.minecraft_version).await?;

    // Merge the loader before planning: its libraries and arguments decide what
    // must be installed and how the process is started.
    let instance = InstanceSpec {
        id: spec.version_name.clone(),
        name: spec.version_name.clone(),
        minecraft: spec.minecraft_version.clone(),
        loader: spec.loader.clone(),
        jvm_args: Vec::new(),
        mods: Vec::new(),
        files: Vec::new(),
        address: spec.auto_connect.clone(),
        auto_connect: spec.auto_connect.is_some(),
        origin: InstanceOrigin::User,
    };
    let effective =
        service::resolve_effective_version(client, &instance, &resolved.version, roots, rule_ctx)
            .await?;
    let effective_version = &effective.version;

    let plan =
        crate::install::planner::plan_vanilla(effective_version, &resolved.assets, roots, rule_ctx);

    // Download anything that is missing before touching natives, so a repair
    // never leaves a half-extracted natives directory behind.
    let report = crate::net::downloader::scan(&plan.files, &mut |_| {}).await?;
    if !report.is_clean() {
        let invalid: Vec<_> = report.invalid().cloned().collect();
        info!(count = invalid.len(), "downloading missing files before launch");
        crate::net::downloader::download_all(client, &invalid, 16, &|_| {}).await?;
    }

    // Fresh natives directory every launch: two versions can ship different
    // builds of the same DLL.
    let natives_base = natives_root(temp_dir);
    purge_stale_natives(&natives_base)?;
    let natives_dir = new_natives_dir(&natives_base)?;
    // Legacy versions read a flat directory; modern ones point each property at a
    // subdirectory, so both layouts are prepared.
    prepare_native_dirs(&natives_dir)?;
    extract_natives(&plan.natives, &natives_dir)?;

    let client_jar =
        if classpath::include_client_jar(&effective_version.id, spec.classpath_policy()) {
            Some(roots.client_jar(&resolved.version.id))
        } else {
            None
        };

    let mojang_entries: Vec<ClasspathEntry> = plan
        .classpath
        .iter()
        .skip(1) // the client jar is handled above
        .map(|path| ClasspathEntry::new(path.clone()))
        .collect();

    let classpath_entries = classpath::build(client_jar.as_deref(), &mojang_entries, &[]);

    let ctx = LaunchContext {
        auth_player_name: spec.account.username.clone(),
        // Forge and Fabric read `${version_name}` to locate their own profile
        // directory, so it must be the loader's version id when one is in use.
        version_name: if spec.loader.kind == LoaderKind::Vanilla {
            spec.version_name.clone()
        } else {
            effective_version.id.clone()
        },
        game_directory: game_directory.to_string_lossy().to_string(),
        assets_root: roots.assets_dir().to_string_lossy().to_string(),
        assets_index_name: effective_version.assets.clone().unwrap_or_default(),
        auth_uuid: spec.account.uuid.clone(),
        auth_access_token: spec.account.access_token.clone(),
        user_type: spec.account.user_type.clone(),
        version_type: effective_version.kind.clone(),
        resolution_width: spec.resolution.map(|(w, _)| w),
        resolution_height: spec.resolution.map(|(_, h)| h),
        natives_directory: natives_dir.to_string_lossy().to_string(),
        launcher_name: crate::LAUNCHER_NAME.to_owned(),
        launcher_version: crate::VERSION.to_owned(),
        classpath: classpath::to_classpath_string(&classpath_entries, classpath_separator()),
        library_directory: roots.libraries_dir().to_string_lossy().to_string(),
        classpath_separator: classpath_separator().to_owned(),
        extra: Vec::new(),
    };

    let mut loader_args = LoaderArguments::default();
    if spec.fullscreen {
        // `--fullscreen` is handled by the profile on 1.13+, but older profiles
        // never declare it, so add it unconditionally when requested.
        if effective_version.arguments.is_none() {
            loader_args.game.push("--fullscreen".to_owned());
            loader_args.game.push("true".to_owned());
        }
    }
    if let Some(address) = &spec.auto_connect {
        // 1.20+ uses quick play; older versions accept --server/--port.
        let (host, port) = match address.rsplit_once(':') {
            Some((host, port)) => (host, port.parse::<u16>().unwrap_or(25565)),
            None => (address.as_str(), 25565),
        };
        if mc_at_least(&resolved.version.id, 1, 20) {
            loader_args.game.push("--quickPlayMultiplayer".to_owned());
            loader_args.game.push(format!("{host}:{port}"));
        } else {
            loader_args.game.push("--server".to_owned());
            loader_args.game.push(host.to_owned());
            loader_args.game.push("--port".to_owned());
            loader_args.game.push(port.to_string());
        }
    }

    let mut arguments = build_arguments(effective_version, &ctx, rule_ctx, &loader_args)?;

    // Java agents and JVM options come first so they apply to the whole VM.
    let mut jvm_prefix: Vec<String> = spec.jvm_options.clone();
    jvm_prefix.extend(spec.java_agents.iter().cloned());
    if !jvm_prefix.is_empty() {
        let main_index = arguments
            .iter()
            .position(|arg| Some(arg) == effective_version.main_class.as_ref())
            .unwrap_or(0);
        let mut merged = Vec::with_capacity(arguments.len() + jvm_prefix.len());
        merged.extend(arguments.drain(..main_index));
        merged.extend(jvm_prefix);
        merged.extend(arguments);
        arguments = merged;
    }

    Ok(PreparedLaunch { arguments, natives_dir, plan })
}

/// Build and spawn the game.
pub async fn launch(
    client: &Client,
    spec: &LaunchSpec,
    roots: &PlanRoots,
    rule_ctx: &RuleContext,
    temp_dir: &Path,
) -> Result<GameProcess> {
    let prepared = build(client, spec, roots, rule_ctx, temp_dir).await?;

    let request = LaunchRequest {
        java_executable: absolute(&spec.java_executable),
        arguments: prepared.arguments,
        working_dir: absolute(&spec.game_directory),
        detached: spec.detached,
    };

    let mut process = process::spawn(request).await?;
    process.natives_dir = Some(prepared.natives_dir);
    Ok(process)
}

fn mc_at_least(version: &str, major: u32, minor: u32) -> bool {
    let mut pieces = version.split('.');
    let current_major: u32 = pieces.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let current_minor: u32 = pieces.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (current_major, current_minor) >= (major, minor)
}

/// Java requirement for a launch, from Mojang's published major.
///
/// The `version.json` for the instance is the only source: release names no longer
/// encode the runtime (`26.3` needs Java 25) and a manifest cannot know better, so
/// there is nothing for a caller to override.
pub async fn java_requirement(
    client: &Client,
    spec: &LaunchSpec,
) -> Result<crate::java::JavaRequirement> {
    let metadata_major = match service::resolve(client, &spec.minecraft_version).await {
        Ok(resolved) => resolved.version.java_version.as_ref().map(|jv| jv.major_version),
        Err(err) => {
            // A resolve failure will surface again from `build`; do not hide the
            // requirement behind a second error here.
            tracing::warn!(error = %err, "could not read the java version from metadata");
            None
        }
    };

    Ok(crate::java::requirement_for_version(metadata_major))
}

/// Choose an installed JVM for a requirement, or download the matching one.
///
/// Returns the executable to run. This is what stops a 1.20.1 instance from being
/// started on Java 25, and what makes `26.3` work by fetching Java 25.
pub async fn ensure_java(
    client: &Client,
    requirement: &crate::java::JavaRequirement,
    installations: &[crate::java::JavaInstallation],
    pinned: Option<&str>,
    java_dir: &Path,
) -> Result<PathBuf> {
    match crate::java::choose_java(installations, requirement, pinned) {
        crate::java::JavaChoice::Installed(java) => Ok(java.executable),
        crate::java::JavaChoice::NeedsDownload { major, reason } => {
            info!(major, %reason, "downloading a matching JDK");
            let download = crate::java::resolve_jdk(client, major).await?;
            crate::java::install_jdk(client, java_dir, &download, major).await
        }
    }
}

/// Make a path absolute without requiring it to exist.
///
/// `std::fs::canonicalize` fails on missing directories, and on Windows it adds
/// a `\\?\` prefix the JVM does not always accept, so this joins the current
/// directory instead and normalises separators.
fn absolute(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }
    std::env::current_dir().map(|cwd| cwd.join(path)).unwrap_or_else(|_| path.to_path_buf())
}

/// Reject a launch spec whose account has no token, with a clear message.
pub fn validate_spec(spec: &LaunchSpec) -> Result<()> {
    if spec.account.username.trim().is_empty() {
        return Err(MangoError::Other("account username is empty".to_owned()));
    }
    if spec.account.access_token.trim().is_empty() {
        return Err(MangoError::Other("account has no access token; sign in first".to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_uuid_is_stable_and_well_formed() {
        let a = offline_uuid("Steve");
        let b = offline_uuid("Steve");
        assert_eq!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(a.chars().filter(|c| *c == '-').count(), 4);
        assert_ne!(offline_uuid("Alex"), a);
    }

    #[test]
    fn version_comparison_handles_snapshots() {
        assert!(mc_at_least("1.20.1", 1, 20));
        assert!(mc_at_least("1.20", 1, 20));
        assert!(!mc_at_least("1.19.4", 1, 20));
        assert!(!mc_at_least("24w01a", 1, 20));
    }

    #[test]
    fn empty_username_or_token_is_rejected() {
        let mut spec = LaunchSpec {
            minecraft_version: "1.20.1".to_owned(),
            version_name: "main".to_owned(),
            game_directory: PathBuf::from("/g"),
            java_executable: PathBuf::from("/java"),
            account: LaunchAccount::offline(""),
            loader: LoaderSpec::default(),
            detached: false,
            resolution: None,
            fullscreen: false,
            jvm_options: Vec::new(),
            java_agents: Vec::new(),
            auto_connect: None,
        };
        assert!(validate_spec(&spec).is_err());

        spec.account = LaunchAccount::offline("Steve");
        spec.account.access_token.clear();
        assert!(validate_spec(&spec).is_err());

        spec.account = LaunchAccount::offline("Steve");
        assert!(validate_spec(&spec).is_ok());
    }
}
