use std::path::PathBuf;

use clap::{Args, ValueEnum};
use mango_core::install::PlanRoots;
use mango_core::java::discover;
use mango_core::launch::service::{self, LaunchAccount, LaunchSpec};
use mango_core::manifest::{LoaderKind, LoaderSpec};
use mango_core::mojang::rules::RuleContext;
use mango_core::net::http;
use mango_core::{MangoError, config::Config, paths::DataPaths};

use crate::commands::install::game_paths;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum LoaderChoice {
    Vanilla,
    Fabric,
    Quilt,
    Forge,
    Neoforge,
}

impl From<LoaderChoice> for LoaderKind {
    fn from(choice: LoaderChoice) -> Self {
        match choice {
            LoaderChoice::Vanilla => LoaderKind::Vanilla,
            LoaderChoice::Fabric => LoaderKind::Fabric,
            LoaderChoice::Quilt => LoaderKind::Quilt,
            LoaderChoice::Forge => LoaderKind::Forge,
            LoaderChoice::Neoforge => LoaderKind::Neoforge,
        }
    }
}

#[derive(Debug, Args)]
pub struct LaunchArgs {
    /// Minecraft version id.
    #[arg(long, default_value = "1.20.1")]
    pub mc: String,

    /// Server instance id (also used as the game directory).
    #[arg(long, default_value = "default")]
    pub server: String,

    /// Player name for an offline profile.
    #[arg(long, default_value = "Player")]
    pub username: String,

    /// Use the selected authenticated account instead of an offline profile.
    #[arg(long)]
    pub online: bool,

    /// Mod loader to install and launch with.
    #[arg(long, value_enum, default_value = "vanilla")]
    pub loader: LoaderChoice,

    /// Loader version; newest compatible is used when omitted.
    #[arg(long)]
    pub loader_version: Option<String>,

    /// Explicit Java executable; auto-discovered when omitted.
    #[arg(long)]
    pub java: Option<PathBuf>,

    /// Do not detach; keep the game attached so its log streams to this shell.
    #[arg(long)]
    pub attached: bool,

    /// Window width.
    #[arg(long, default_value_t = 1280)]
    pub width: u32,

    /// Window height.
    #[arg(long, default_value_t = 720)]
    pub height: u32,

    /// Launch fullscreen.
    #[arg(long)]
    pub fullscreen: bool,

    /// Only print the resolved command line; do not start the game.
    #[arg(long)]
    pub dry_run: bool,
}

pub async fn run(args: LaunchArgs, data: &DataPaths, config: &Config) -> mango_core::Result<()> {
    let game = game_paths(data, config);
    let roots = PlanRoots::new(game.common_dir(), game.instance_dir(&args.server));
    let client = http::build_client()?;

    // `--online` uses the selected account and its tokens; without it the launch
    // uses a local offline profile, which is what development and LAN play need.
    let (account, java_agents) = if args.online {
        let uuid = config.selected_account.clone().ok_or_else(|| {
            MangoError::Other("no account selected; run `mango auth login`".to_owned())
        })?;
        let mut config = config.clone();
        let (account, agents) =
            mango_core::auth::launch_profile(&client, data, &mut config, &uuid).await?;
        if !agents.is_empty() {
            println!("authlib  {} extra JVM argument(s)", agents.len());
        }
        (account, agents)
    } else {
        (LaunchAccount::offline(&args.username), Vec::new())
    };

    let spec = LaunchSpec {
        minecraft_version: args.mc.clone(),
        version_name: args.server.clone(),
        game_directory: roots.instance_dir.clone(),
        // Filled in below once the JVM is chosen.
        java_executable: PathBuf::new(),
        account,
        loader: LoaderSpec { kind: args.loader.into(), version: args.loader_version.clone() },
        detached: !args.attached,
        resolution: Some((args.width, args.height)),
        fullscreen: args.fullscreen,
        jvm_options: vec!["-Xmx2G".to_owned(), "-Xms2G".to_owned()],
        java_agents,
        auto_connect: None,
    };
    service::validate_spec(&spec)?;

    // The required runtime comes from the version metadata, never from the
    // version name: `26.3` needs Java 25 and 1.20.1 needs 17.
    let requirement = service::java_requirement(&client, &spec).await?;
    println!("needs    Java {} ({})", requirement.suggested, requirement.supported);

    let installations = discover::discover();
    let java = match args.java.clone() {
        Some(path) => path,
        None => {
            let java_dir = data.root().join("java");
            service::ensure_java(&client, &requirement, &installations, None, &java_dir).await?
        }
    };

    let probed = discover::probe(&java)?;
    println!("using    {} ({})", probed.version, probed.vendor);

    let spec = LaunchSpec { java_executable: java, ..spec };
    let temp = std::env::temp_dir();

    if args.dry_run {
        let prepared = service::build(&client, &spec, &roots, &RuleContext::new(), &temp).await?;
        println!();
        println!("game dir   {}", spec.game_directory.display());
        println!("natives    {}", prepared.natives_dir.display());
        println!("files      {}", prepared.plan.files.len());
        println!();
        println!("command:");
        print!("  {}", spec.java_executable.display());
        for argument in &prepared.arguments {
            print!(" {argument}");
        }
        println!();
        // Natives were extracted for the dry run; clean them up.
        std::fs::remove_dir_all(&prepared.natives_dir).ok();
        return Ok(());
    }

    let process = service::launch(&client, &spec, &roots, &RuleContext::new(), &temp).await?;
    println!("launched pid {:?}", process.pid());

    if args.attached {
        let code = process.wait().await?;
        println!("game exited with {code:?}");
    } else {
        println!("game detached; the launcher may exit");
        // The child keeps running on its own; drop our handle without killing it.
        std::mem::forget(process);
    }

    Ok(())
}

/// Launch a stored instance, optionally attaching to its console.
pub async fn launch_instance_with(
    instance: &mango_core::instance::UserInstance,
    username: &str,
    data: &DataPaths,
    config: &Config,
    detached: bool,
) -> mango_core::Result<()> {
    let game = game_paths(data, config);
    let instance_dir = game.instances_dir().join(&instance.id);
    let roots = PlanRoots::new(game.common_dir(), instance_dir.clone());
    let client = http::build_client()?;

    // Prefer the signed-in account; fall back to the offline name so this still
    // works before anyone has authenticated.
    let (account, java_agents) = match config.selected_account.clone() {
        Some(uuid) if config.accounts.contains_key(&uuid) => {
            let mut config = config.clone();
            match mango_core::auth::launch_profile(&client, data, &mut config, &uuid).await {
                Ok((account, agents)) => {
                    if !agents.is_empty() {
                        println!("authlib  {} extra JVM argument(s)", agents.len());
                    }
                    (account, agents)
                }
                Err(err) => {
                    println!("auth     {err}");
                    println!("auth     falling back to the offline profile");
                    (LaunchAccount::offline(username), Vec::new())
                }
            }
        }
        _ => (LaunchAccount::offline(username), Vec::new()),
    };

    let spec = LaunchSpec {
        minecraft_version: instance.minecraft.clone(),
        version_name: instance.id.clone(),
        game_directory: instance_dir,
        // Filled in below once the JVM is chosen.
        java_executable: PathBuf::new(),
        account,
        loader: instance.loader.clone(),
        detached,
        resolution: Some((1280, 720)),
        fullscreen: false,
        jvm_options: instance.jvm_args.clone(),
        java_agents,
        auto_connect: None,
    };
    service::validate_spec(&spec)?;

    // Ask the version metadata which Java this release needs.
    let requirement = service::java_requirement(&client, &spec).await?;
    println!("needs    Java {} ({})", requirement.suggested, requirement.supported);

    let installations = discover::discover();
    let java_dir = data.root().join("java");
    let java = service::ensure_java(&client, &requirement, &installations, None, &java_dir).await?;

    let probed = discover::probe(&java)?;
    println!("using    {} ({})", probed.version, probed.vendor);

    let spec = LaunchSpec { java_executable: java, ..spec };
    let process =
        service::launch(&client, &spec, &roots, &RuleContext::new(), &std::env::temp_dir()).await?;
    println!("launched pid {:?}", process.pid());

    if detached {
        std::mem::forget(process);
    } else {
        let code = process.wait().await?;
        println!("game exited with {code:?}");
    }
    Ok(())
}
