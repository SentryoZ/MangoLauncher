//! High-level installation flow: fetch metadata, plan, verify, download.
//!
//! This is the module the CLI and the UI both call through, so the two never
//! drift apart in behaviour.

use reqwest::Client;
use tracing::{debug, info, warn};

use crate::error::{MangoError, Result};
use crate::install::lockfile;
use crate::install::planner::{InstallPlan, PlanRoots, add_manifest_files, plan_vanilla};
use crate::instance::InstanceSpec;
use crate::manifest::Lockfile;
use crate::mojang::rules::RuleContext;
use crate::mojang::{AssetIndex, VersionJson, VersionManifest};
use crate::net::downloader::{self, FileKind, PlannedFile, Progress, ScanReport};
use crate::net::http;

/// Mojang metadata endpoints.
pub const VERSION_MANIFEST_URL: &str =
    "https://piston-meta.mojang.com/mc/game/version_manifest_v2.json";

/// Resolve a version id to its `version.json` URL.
pub async fn resolve_version_url(client: &Client, version_id: &str) -> Result<String> {
    let manifest: VersionManifest = http::get_json(client, VERSION_MANIFEST_URL).await?;
    manifest
        .find(version_id)
        .map(|summary| summary.url.clone())
        .ok_or_else(|| MangoError::Other(format!("unknown Minecraft version `{version_id}`")))
}

/// Download and parse a `version.json`.
pub async fn fetch_version_json(client: &Client, url: &str) -> Result<VersionJson> {
    let bytes = http::get_bytes(client, url).await?;
    VersionJson::parse(&bytes)
}

/// Download and parse the asset index referenced by a version.
pub async fn fetch_asset_index(client: &Client, version: &VersionJson) -> Result<AssetIndex> {
    let reference = version
        .asset_index
        .as_ref()
        .ok_or_else(|| MangoError::Other(format!("version {} has no asset index", version.id)))?;
    let bytes = http::get_bytes(client, &reference.url).await?;
    AssetIndex::parse(&bytes)
}

/// Metadata needed to launch, grouped so callers hold it once.
#[derive(Debug, Clone)]
pub struct ResolvedVersion {
    pub version: VersionJson,
    pub assets: AssetIndex,
}

/// Fetch everything needed to plan an install.
pub async fn resolve(client: &Client, version_id: &str) -> Result<ResolvedVersion> {
    let url = resolve_version_url(client, version_id).await?;
    let version = fetch_version_json(client, &url).await?;
    let assets = fetch_asset_index(client, &version).await?;
    Ok(ResolvedVersion { version, assets })
}

/// Outcome of an install run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallOutcome {
    pub plan: InstallPlan,
    pub report: ScanReport,
    pub downloaded: usize,
    /// Mods resolved from an upstream catalogue, empty for vanilla installs.
    pub mods: crate::mods::ResolveReport,
}

/// Caller-provided knobs for an install.
#[derive(Debug, Clone, Default)]
pub struct InstallOptions {
    /// CurseForge API key. CurseForge refuses every request without one.
    pub curseforge_key: Option<String>,
    /// Per-mod overrides keyed by project id, from the player's settings.
    ///
    /// An explicit `true` enables a mod that is off by default; `false` disables
    /// an optional mod that is on by default. Absent means "use the manifest's
    /// default".
    pub mod_overrides: std::collections::BTreeMap<String, bool>,
}

impl InstallOptions {
    /// Whether an entry should be installed for this player.
    ///
    /// Required mods are always installed: the server disconnects a client that
    /// is missing them, so a toggle there would only produce a broken launch.
    pub fn is_mod_enabled(&self, entry: &crate::manifest::ModEntry) -> bool {
        if entry.required {
            return true;
        }
        match self.mod_overrides.get(&entry.id) {
            Some(explicit) => *explicit,
            None => !entry.disabled_by_default,
        }
    }
}

/// Plan, verify and (when needed) download a vanilla installation.
pub async fn install_vanilla(
    client: &Client,
    version_id: &str,
    roots: &PlanRoots,
    ctx: &RuleContext,
    progress: &impl Fn(Progress),
) -> Result<InstallOutcome> {
    let resolved = resolve(client, version_id).await?;
    let plan = plan_vanilla(&resolved.version, &resolved.assets, roots, ctx);
    apply_plan(client, plan, progress).await
}

/// Plan, verify and download everything an instance needs.
///
/// Works for both manifest servers and player-created instances: both reduce to
/// an [`InstanceSpec`], so there is one install path rather than two that can
/// drift apart. When the instance declares a mod loader, its profile is resolved
/// first and the vanilla version merged with it. Mods are then resolved from
/// their catalogue, which pins `latest` to a concrete file.
pub async fn install_instance(
    client: &Client,
    instance: &InstanceSpec,
    roots: &PlanRoots,
    ctx: &RuleContext,
    manifest_version: Option<&str>,
    options: &InstallOptions,
    progress: &impl Fn(Progress),
) -> Result<InstallOutcome> {
    let resolved = resolve(client, &instance.minecraft).await?;
    let effective =
        resolve_effective_version(client, instance, &resolved.version, roots, ctx).await?;
    let mut plan = plan_vanilla(&effective.version, &resolved.assets, roots, ctx);
    add_manifest_files(&mut plan, &instance.files, &roots.instance_dir);

    // Resolve mods before the plan runs so a missing required mod fails before
    // hundreds of megabytes are downloaded.
    let mods = resolve_mods(client, instance, options, roots).await?;
    for modification in &mods.resolved {
        plan.files.push(PlannedFile {
            kind: FileKind::Mod,
            path: roots.instance_dir.join("mods").join(&modification.file_name),
            url: modification.url.clone(),
            size: modification.size,
            hash: modification.hash.clone(),
        });
    }

    let outcome = apply_plan(client, plan, progress).await?;

    let mut lockfile = lockfile::from_plan(&outcome.plan, instance, roots);
    lockfile.manifest_version = manifest_version.map(str::to_owned);
    lockfile.version_url = Some(resolve_version_url(client, &instance.minecraft).await?);

    // Record the *resolved* loader version, not the manifest's placeholder, so
    // the lockfile pins what was actually installed.
    lockfile::apply_loader(&mut lockfile, &instance.loader, effective.loader_version.as_deref());
    lockfile.mods = mods
        .resolved
        .iter()
        .map(|modification| crate::manifest::LockedMod {
            source: match modification.source {
                crate::manifest::ModSource::Modrinth => "modrinth".to_owned(),
                crate::manifest::ModSource::Curseforge => "curseforge".to_owned(),
            },
            id: modification.project_id.clone(),
            version_id: modification.version_id.clone(),
            file_name: modification.file_name.clone(),
            sha512: modification.hash.clone().map(|hash| hash.value),
            required: modification.required,
        })
        .collect();

    let path = lockfile.save(&roots.instance_dir)?;
    info!(path = %path.display(), "lockfile written");

    Ok(InstallOutcome { mods, ..outcome })
}

/// Resolve the enabled mods for an instance.
async fn resolve_mods(
    client: &Client,
    instance: &InstanceSpec,
    options: &InstallOptions,
    roots: &PlanRoots,
) -> Result<crate::mods::ResolveReport> {
    let enabled: Vec<_> =
        instance.mods.iter().filter(|entry| options.is_mod_enabled(entry)).cloned().collect();

    if enabled.is_empty() {
        return Ok(crate::mods::ResolveReport::default());
    }

    info!(count = enabled.len(), "resolving mods");
    let report = crate::mods::resolve_all(
        client,
        &enabled,
        &instance.minecraft,
        instance.loader.kind,
        options.curseforge_key.as_deref(),
    )
    .await?;

    for failure in &report.failures {
        warn!(id = %failure.id, reason = %failure.reason, "mod could not be resolved");
    }

    reconcile_mods_dir(roots, &report)
}

/// Remove jars a previous install left behind that this one does not want.
///
/// Only files matching a resolved mod's name stem are considered, so jars the
/// player dropped in by hand survive. Without this, changing a mod's version
/// leaves both jars in `mods/` and the loader complains about duplicates.
fn reconcile_mods_dir(
    roots: &PlanRoots,
    report: &crate::mods::ResolveReport,
) -> Result<crate::mods::ResolveReport> {
    let mods_dir = roots.instance_dir.join("mods");
    if !mods_dir.exists() {
        return Ok(report.clone());
    }

    let keep: Vec<&str> = report.resolved.iter().map(|entry| entry.file_name.as_str()).collect();

    let Ok(entries) = std::fs::read_dir(&mods_dir) else { return Ok(report.clone()) };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !(name.ends_with(".jar") || name.ends_with(".jar.disabled")) {
            continue;
        }
        if keep.contains(&name.as_str()) {
            continue;
        }

        // Only delete when the file belongs to a mod this install manages, which
        // is recognised by the project's file-name stem appearing in the file.
        let managed = report.resolved.iter().any(|resolved| {
            let stem = resolved.file_name.trim_end_matches(".jar");
            let prefix: String = stem.chars().take_while(|c| *c != '-').collect();
            !prefix.is_empty() && name.starts_with(&prefix)
        });
        if managed {
            debug!(file = %name, "removing stale mod");
            std::fs::remove_file(entry.path()).ok();
        }
    }

    Ok(report.clone())
}

/// A version after merging the server's mod loader.
#[derive(Debug, Clone)]
pub struct EffectiveVersion {
    pub version: VersionJson,
    /// Loader version actually resolved, which pins `latest` in the lockfile.
    pub loader_version: Option<String>,
}

/// Merge an instance's mod loader into the vanilla version.
///
/// Vanilla instances short-circuit: no network calls and the input is returned
/// unchanged.
pub async fn resolve_effective_version(
    client: &Client,
    instance: &InstanceSpec,
    vanilla: &VersionJson,
    roots: &PlanRoots,
    ctx: &RuleContext,
) -> Result<EffectiveVersion> {
    if instance.loader.kind == crate::manifest::LoaderKind::Vanilla {
        return Ok(EffectiveVersion { version: vanilla.clone(), loader_version: None });
    }

    let profile = crate::loaders::resolve(
        client,
        &roots.common_dir,
        instance.loader.kind,
        &instance.minecraft,
        instance.loader.version.as_deref(),
    )
    .await?;

    info!(
        loader = instance.loader.kind.as_str(),
        profile = %profile.version_id,
        "merging loader profile"
    );

    // Loaders publish their own version in the profile id; prefer the requested
    // value when set, otherwise take it from what the loader resolved to.
    let loader_version = instance.loader.version.clone().or_else(|| {
        profile
            .version_id
            .split(['-', '+'])
            .find(|part| part.chars().next().is_some_and(|c| c.is_ascii_digit()))
            .map(str::to_owned)
    });

    Ok(EffectiveVersion {
        version: crate::loaders::merge(vanilla, &profile.profile, ctx),
        loader_version,
    })
}

/// Verify a plan and download only the invalid files.
async fn apply_plan(
    client: &Client,
    plan: InstallPlan,
    progress: &impl Fn(Progress),
) -> Result<InstallOutcome> {
    info!(files = plan.files.len(), bytes = plan.total_bytes(), "install plan ready");

    let report = downloader::scan(&plan.files, &mut |event| progress(event)).await?;
    let invalid: Vec<_> = report.invalid().cloned().collect();

    if invalid.is_empty() {
        info!(present = report.present, "installation already complete");
        progress(Progress::Finished { downloaded: 0, skipped: report.present });
        return Ok(InstallOutcome {
            plan,
            report,
            downloaded: 0,
            mods: crate::mods::ResolveReport::default(),
        });
    }

    warn!(missing = report.missing.len(), corrupt = report.corrupt.len(), "repairing installation");
    let downloaded = downloader::download_all(client, &invalid, 16, progress).await?;

    // Re-verify so a silent truncation cannot be reported as success.
    let verify = downloader::scan(&plan.files, &mut |_| {}).await?;
    if !verify.is_clean() {
        return Err(MangoError::Other(format!(
            "installation still invalid after download: {} missing, {} corrupt",
            verify.missing.len(),
            verify.corrupt.len()
        )));
    }

    Ok(InstallOutcome { plan, report, downloaded, mods: crate::mods::ResolveReport::default() })
}

/// Reuse an existing lockfile when it still matches, skipping a manifest resolve.
pub fn load_matching_lock(
    instance_dir: &std::path::Path,
    manifest_version: Option<&str>,
) -> Option<Lockfile> {
    Lockfile::load(instance_dir)
        .ok()
        .flatten()
        .filter(|lock| lock.matches_manifest(manifest_version))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_url_is_https() {
        assert!(VERSION_MANIFEST_URL.starts_with("https://"));
    }
}
