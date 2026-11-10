use std::path::PathBuf;

use clap::Subcommand;
use mango_core::java::{adoptium, discover, version};
use mango_core::net::http;
use mango_core::paths::DataPaths;

#[derive(Debug, Subcommand)]
pub enum JavaCommand {
    /// List every JVM found on this machine.
    List,
    /// Download and extract a Temurin JDK.
    Install {
        /// Java major version, e.g. 17.
        major: u32,
    },
    /// Show which JVM would be used for a version range.
    Resolve {
        /// Semver range from a manifest, e.g. ">=17".
        #[arg(long)]
        supported: Option<String>,
        /// Suggested major version.
        #[arg(long)]
        suggested: Option<u32>,
    },
}

pub async fn run(command: JavaCommand, data: &DataPaths) -> mango_core::Result<()> {
    match command {
        JavaCommand::List => list(),
        JavaCommand::Install { major } => install(major, data).await,
        JavaCommand::Resolve { supported, suggested } => resolve(supported, suggested),
    }
}

fn list() -> mango_core::Result<()> {
    let installs = discover::discover();
    if installs.is_empty() {
        println!("no java installations found");
        return Ok(());
    }

    for install in installs {
        println!(
            "{:<8} {:<12} {:<10} {}",
            install.version,
            install.vendor,
            install.arch,
            install.executable.display()
        );
    }
    Ok(())
}

fn resolve(supported: Option<String>, suggested: Option<u32>) -> mango_core::Result<()> {
    let installs = discover::discover();
    let requirement = supported.as_deref().map(|range| (range, suggested.unwrap_or(0)));

    match discover::best_match(&installs, requirement) {
        Some(chosen) => {
            println!("{}", chosen.executable.display());
            println!("version {}", chosen.version);
            if let Ok(normalized) = version::normalize(&chosen.version) {
                println!("major   {}", version::major(&normalized));
            }
        }
        None => {
            println!("no suitable java found");
        }
    }
    Ok(())
}

async fn install(major: u32, data: &DataPaths) -> mango_core::Result<()> {
    let client = http::build_client()?;
    let download = adoptium::resolve(&client, major).await?;

    println!("resolved JDK {} ({})", download.version, download.file_name);
    println!("url {}", download.url);

    let java_dir = data.root().join("java");
    let executable = adoptium::install(&client, &java_dir, &download, major).await?;

    println!("installed {}", executable.display());
    let install = discover::probe(&executable)?;
    println!("version {} ({})", install.version, install.vendor);
    println!("dir     {}", PathBuf::from(&executable).parent().unwrap_or(&java_dir).display());
    Ok(())
}
