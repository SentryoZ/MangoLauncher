//! `mango` — command line interface for MangoLauncher.
//!
//! Everything the GUI can do must be reachable here first, so the core can be
//! exercised without a display server.

mod commands;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use mango_core::paths::{DataPaths, default_root};
use mango_core::{config::Config, logging};

#[derive(Debug, Parser)]
#[command(name = "mango", version, about = "Mango Minecraft launcher", long_about = None)]
struct Cli {
    /// Override the launcher data directory (config, logs, caches).
    #[arg(long, global = true, env = "MANGO_DATA_DIR")]
    data_dir: Option<PathBuf>,

    /// Enable debug logging.
    #[arg(long, global = true, short)]
    verbose: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print the resolved paths and current settings.
    Info,
    /// Print the effective configuration as JSON.
    Config,
    /// Download and verify a Minecraft installation.
    Install(commands::install::InstallArgs),
    /// Inspect and provision Java runtimes.
    Java {
        #[command(subcommand)]
        command: commands::java::JavaCommand,
    },
    /// Install if needed, then start Minecraft.
    Launch(commands::launch::LaunchArgs),
    /// Manage accounts and authentication.
    Auth {
        #[command(subcommand)]
        command: commands::auth::AuthCommand,
    },
    /// Manage player-created instances.
    Instance {
        #[command(subcommand)]
        command: commands::instance::InstanceCommand,
    },
    /// Install a stored instance, optionally launching it.
    Run {
        /// Instance id.
        id: String,
        /// Launch after installing.
        #[arg(long)]
        launch: bool,
        /// Keep the game attached so its log streams here.
        #[arg(long)]
        attached: bool,
        /// Offline player name.
        #[arg(long, default_value = "Player")]
        username: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();

    let data_paths = match cli.data_dir.clone() {
        Some(dir) => DataPaths::new(dir),
        None => match default_root() {
            Ok(root) => DataPaths::new(root),
            Err(err) => {
                eprintln!("error: {err}");
                return ExitCode::FAILURE;
            }
        },
    };

    let _logging = match logging::init(data_paths.root(), cli.verbose) {
        Ok(logging) => logging,
        Err(err) => {
            eprintln!("error: could not start logging: {err}");
            return ExitCode::FAILURE;
        }
    };

    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("error: could not start async runtime: {err}");
            return ExitCode::FAILURE;
        }
    };

    match runtime.block_on(run(cli, data_paths)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli, data_paths: DataPaths) -> mango_core::Result<()> {
    let mut config = Config::load(&data_paths)?;

    match cli.command {
        Command::Info => {
            let game = commands::install::game_paths(&data_paths, &config);
            commands::info::run(&data_paths, &game, &config)
        }
        Command::Config => commands::config::run(&config),
        Command::Install(args) => commands::install::run(args, &data_paths, &config).await,
        Command::Java { command } => commands::java::run(command, &data_paths).await,
        Command::Launch(args) => commands::launch::run(args, &data_paths, &config).await,
        Command::Auth { command } => commands::auth::run(command, &data_paths, &mut config).await,
        Command::Instance { command } => {
            commands::instance::run(command, &data_paths, &config).await
        }
        Command::Run { id, launch, attached, username } => {
            commands::instance::install_with(
                &id,
                launch,
                &username,
                &data_paths,
                &config,
                !attached,
            )
            .await
        }
    }
}
