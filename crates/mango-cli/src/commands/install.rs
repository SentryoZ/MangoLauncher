use std::path::PathBuf;

use clap::{Args, ValueEnum};
use mango_core::install::{PlanRoots, install_instance, install_vanilla};
use mango_core::manifest::{self, FetchedManifest, ManifestOrigin};
use mango_core::mojang::RuleContext;
use mango_core::net::http;
use mango_core::{
    MangoError,
    config::Config,
    paths::{DataPaths, GamePaths},
};

use crate::commands::progress_reporter::ProgressReporter;

/// Resolve the game installation root from configuration.
pub fn game_paths(data: &DataPaths, config: &Config) -> GamePaths {
    GamePaths::new(
        config
            .settings
            .launcher
            .data_directory
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| data.root().to_path_buf()),
    )
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum VersionKind {
    /// Pin an exact version id.
    Exact,
    /// Newest release.
    Release,
    /// Newest snapshot.
    Snapshot,
}

#[derive(Debug, Args)]
pub struct InstallArgs {
    /// Minecraft version id, e.g. 1.20.1. Ignored when --manifest is used.
    #[arg(long, default_value = "1.20.1")]
    pub mc: String,

    /// How `--mc` should be interpreted beyond an exact id.
    #[arg(long, value_enum, default_value = "exact")]
    pub kind: VersionKind,

    /// Manifest path or URL. Installs a manifest server instead of a bare version.
    #[arg(long)]
    pub manifest: Option<String>,

    /// Server id inside the manifest; defaults to mainServer.
    #[arg(long)]
    pub server: Option<String>,

    /// Server instance id used when no manifest is given.
    #[arg(long, default_value = "default")]
    pub instance: String,

    /// Refuse to proceed when no lockfile matches the manifest.
    #[arg(long)]
    pub locked: bool,

    /// Do not fall back to the cached manifest when the network fails.
    #[arg(long)]
    pub no_offline: bool,

    /// Install into a standalone directory instead of the configured one.
    #[arg(long)]
    pub common_dir: Option<PathBuf>,
}

pub async fn run(args: InstallArgs, data: &DataPaths, config: &Config) -> mango_core::Result<()> {
    let game = game_paths(data, config);
    let common_dir = args.common_dir.clone().unwrap_or_else(|| game.common_dir());
    let client = http::build_client()?;
    let reporter = ProgressReporter::new();

    let outcome = match &args.manifest {
        Some(source) => {
            let fetched = load_manifest(&client, source, data, !args.no_offline).await?;
            report_origin(&fetched);

            let server = fetched.manifest.resolve_server(args.server.as_deref())?;
            println!("server    {} ({})", server.name, server.id);
            println!("minecraft {}", server.minecraft);
            println!("loader    {}", server.loader.kind.as_str());

            let instance_dir = game.instance_dir(&server.id);
            let roots = PlanRoots::new(common_dir.clone(), instance_dir.clone());

            let existing = mango_core::install::service::load_matching_lock(
                &instance_dir,
                fetched.manifest.updated_at.as_deref(),
            );
            if args.locked && existing.is_none() {
                return Err(MangoError::Other(
                    "no lockfile matches this manifest; run without --locked to create one"
                        .to_owned(),
                ));
            }
            if let Some(lock) = &existing {
                println!("lockfile  {} files pinned", lock.files.len());
            }

            install_instance(
                &client,
                &server.to_instance_spec(),
                &roots,
                &RuleContext::new(),
                fetched.manifest.updated_at.as_deref(),
                &mango_core::install::InstallOptions::default(),
                &|event| reporter.on(event),
            )
            .await?
        }
        None => {
            let version = resolve_version_id(&client, &args.mc, args.kind).await?;
            let roots = PlanRoots::new(common_dir.clone(), game.instance_dir(&args.instance));
            install_vanilla(&client, &version, &roots, &RuleContext::new(), &|event| {
                reporter.on(event)
            })
            .await?
        }
    };

    reporter.finish();

    println!();
    println!("version        {}", outcome.plan.base_version);
    println!("files          {}", outcome.plan.files.len());
    println!("downloaded     {}", outcome.downloaded);
    println!("already valid  {}", outcome.report.present);
    println!("classpath      {} entries", outcome.plan.classpath.len());
    println!("natives        {} archives", outcome.plan.natives.len());

    if !outcome.report.is_clean() && outcome.downloaded == 0 {
        println!();
        println!("nothing was downloaded; run again to repair");
    }

    Ok(())
}

/// Read a manifest from disk or over HTTP, with cache fallback.
async fn load_manifest(
    client: &mango_core::net::http::Client,
    source: &str,
    data: &DataPaths,
    allow_offline: bool,
) -> mango_core::Result<FetchedManifest> {
    if source.starts_with("http://") || source.starts_with("https://") {
        let cache = manifest::cache_path_for(data.root(), source);
        manifest::fetch(client, source, &cache, allow_offline).await
    } else {
        manifest::load_local(std::path::Path::new(source))
    }
}

fn report_origin(fetched: &FetchedManifest) {
    let text = match fetched.origin {
        ManifestOrigin::Network => "downloaded",
        ManifestOrigin::NotModified => "cache (unchanged)",
        ManifestOrigin::Cache => "cache (offline)",
        ManifestOrigin::LocalFile => "local file",
    };
    println!("manifest  {} — {}", fetched.manifest.name, text);
}

async fn resolve_version_id(
    client: &mango_core::net::http::Client,
    mc: &str,
    kind: VersionKind,
) -> mango_core::Result<String> {
    match kind {
        VersionKind::Exact => Ok(mc.to_owned()),
        VersionKind::Release | VersionKind::Snapshot => {
            let manifest: mango_core::mojang::VersionManifest =
                http::get_json(client, mango_core::install::service::VERSION_MANIFEST_URL).await?;
            let kind = if matches!(kind, VersionKind::Release) { "release" } else { "snapshot" };
            manifest
                .newest_of_kind(kind)
                .map(|v| v.id.clone())
                .ok_or_else(|| MangoError::Other(format!("no {kind} version found")))
        }
    }
}
