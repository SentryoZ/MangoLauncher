use std::path::Path;

use clap::{Args, Subcommand};
use mango_core::instance::{self, UserInstance};
use mango_core::manifest::{LoaderKind, LoaderSpec};
use mango_core::net::http;
use mango_core::{MangoError, config::Config, paths::DataPaths};

use crate::commands::install::game_paths;

#[derive(Debug, Subcommand)]
pub enum InstanceCommand {
    /// List every player-created instance.
    List,
    /// Create an instance for any Minecraft version and loader.
    Create(CreateArgs),
    /// Delete an instance and its game directory.
    Delete {
        /// Instance id.
        id: String,
        /// Required: this removes worlds and configs, not just the recipe.
        #[arg(long)]
        yes: bool,
    },
    /// Print one instance's definition.
    Show { id: String },
    /// List Minecraft versions available to install.
    Versions {
        /// Show snapshots as well as releases.
        #[arg(long)]
        snapshots: bool,
        /// Only show ids containing this text.
        #[arg(long)]
        filter: Option<String>,
    },
}

#[derive(Debug, Args)]
pub struct CreateArgs {
    /// Display name.
    pub name: String,

    /// Minecraft version id, e.g. 1.20.1.
    #[arg(long)]
    pub mc: String,

    /// Mod loader.
    #[arg(long, value_enum, default_value = "vanilla")]
    pub loader: crate::commands::launch::LoaderChoice,

    /// Loader version; newest compatible is used when omitted.
    #[arg(long)]
    pub loader_version: Option<String>,

    /// Directory name; derived from the name when omitted.
    #[arg(long)]
    pub id: Option<String>,
}

pub async fn run(
    command: InstanceCommand,
    data: &DataPaths,
    config: &Config,
) -> mango_core::Result<()> {
    let game = game_paths(data, config);
    let instances_dir = game.instances_dir();

    match command {
        InstanceCommand::List => list(&instances_dir),
        InstanceCommand::Create(args) => create(args, &instances_dir),
        InstanceCommand::Delete { id, yes } => delete(&instances_dir, &id, yes),
        InstanceCommand::Show { id } => show(&instances_dir, &id),
        InstanceCommand::Versions { snapshots, filter } => {
            versions(snapshots, filter.as_deref()).await
        }
    }
}

fn list(instances_dir: &Path) -> mango_core::Result<()> {
    let instances = instance::list_user_instances(instances_dir);
    if instances.is_empty() {
        println!("no instances yet; create one with `mango instance create <name> --mc 1.20.1`");
        return Ok(());
    }

    println!("{:<20} {:<28} {:<10} LOADER", "ID", "NAME", "VERSION");
    for instance in instances {
        let loader = match instance.loader.version.as_deref() {
            Some(version) => format!("{} {version}", instance.loader.kind.as_str()),
            None => instance.loader.kind.as_str().to_owned(),
        };
        println!("{:<20} {:<28} {:<10} {}", instance.id, instance.name, instance.minecraft, loader);
    }
    Ok(())
}

fn create(args: CreateArgs, instances_dir: &Path) -> mango_core::Result<()> {
    let existing: Vec<String> =
        instance::list_user_instances(instances_dir).into_iter().map(|i| i.id).collect();

    let id = args.id.unwrap_or_else(|| instance::suggest_id(&args.name, &existing));

    let loader = LoaderSpec { kind: LoaderKind::from(args.loader), version: args.loader_version };

    let new_instance = UserInstance::new(&id, &args.name, &args.mc, loader)?;
    let path = new_instance.save(instances_dir)?;

    println!("created {}", new_instance.id);
    println!("minecraft {}", new_instance.minecraft);
    println!("loader    {}", new_instance.loader.kind.as_str());
    println!("file      {}", path.display());
    println!();
    println!("install with: mango run {} --launch", new_instance.id);
    Ok(())
}

fn delete(instances_dir: &Path, id: &str, confirmed: bool) -> mango_core::Result<()> {
    if !confirmed {
        return Err(MangoError::Other(format!(
            "refusing to delete `{id}` without --yes: this removes worlds and configs"
        )));
    }
    UserInstance::delete(instances_dir, id)?;
    println!("deleted {id}");
    Ok(())
}

fn show(instances_dir: &Path, id: &str) -> mango_core::Result<()> {
    let dir = instances_dir.join(id);
    let found = UserInstance::load(&dir)?
        .ok_or_else(|| MangoError::Other(format!("no instance with id `{id}`")))?;
    println!("{}", serde_json::to_string_pretty(&found)?);
    Ok(())
}

async fn versions(include_snapshots: bool, filter: Option<&str>) -> mango_core::Result<()> {
    let client = http::build_client()?;
    let manifest: mango_core::mojang::VersionManifest =
        http::get_json(&client, mango_core::install::service::VERSION_MANIFEST_URL).await?;

    let mut shown = 0;
    for version in &manifest.versions {
        if !include_snapshots && version.kind != "release" {
            continue;
        }
        if let Some(filter) = filter
            && !version.id.contains(filter)
        {
            continue;
        }
        let newest = if version.id == manifest.latest.release { "  (latest)" } else { "" };
        println!("{:<16} {:<10}{newest}", version.id, version.kind);
        shown += 1;
    }

    println!();
    println!("{shown} versions");
    Ok(())
}

/// Install a stored instance, optionally attaching it to this console.
pub async fn install_with(
    id: &str,
    launch: bool,
    username: &str,
    data: &DataPaths,
    config: &Config,
    detached: bool,
) -> mango_core::Result<()> {
    let game = game_paths(data, config);
    let instances_dir = game.instances_dir();
    let dir = instances_dir.join(id);
    let found = UserInstance::load(&dir)?
        .ok_or_else(|| MangoError::Other(format!("no instance with id `{id}`")))?;

    let roots = mango_core::install::PlanRoots::new(game.common_dir(), dir.clone());
    let client = http::build_client()?;
    let reporter = crate::commands::progress_reporter::ProgressReporter::new();

    let outcome = mango_core::install::install_instance(
        &client,
        &found.to_spec(),
        &roots,
        &mango_core::mojang::RuleContext::new(),
        None,
        &mango_core::install::InstallOptions::default(),
        &|event| reporter.on(event),
    )
    .await?;
    reporter.finish();

    println!("{}: {} files, {} downloaded", found.id, outcome.plan.files.len(), outcome.downloaded);

    if launch {
        crate::commands::launch::launch_instance_with(&found, username, data, config, detached)
            .await?;
    }
    Ok(())
}
