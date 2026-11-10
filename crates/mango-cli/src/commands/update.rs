//! `mango update`: check for, download, and apply a launcher update.

use clap::Args;

use mango_core::net::http;
use mango_core::paths::DataPaths;
use mango_core::updater;

#[derive(Debug, Args)]
pub struct UpdateArgs {
    /// Release manifest to read. Defaults to the project's latest release.
    #[arg(long)]
    pub manifest: Option<String>,

    /// Only report whether an update exists.
    #[arg(long)]
    pub check: bool,

    /// Offer prereleases as well.
    #[arg(long)]
    pub prerelease: bool,

    /// Download the update without applying it.
    #[arg(long)]
    pub download_only: bool,
}

pub async fn run(args: UpdateArgs, data: &DataPaths) -> mango_core::Result<()> {
    let manifest_url = args.manifest.unwrap_or_else(updater::default_manifest_url);
    let client = http::build_client()?;

    println!("installed  {}", mango_core::VERSION);
    println!("checking   {manifest_url}");

    let Some(update) = updater::check(&client, &manifest_url, args.prerelease).await? else {
        println!("up to date");
        return Ok(());
    };

    println!("available  {}", update.version);
    if let Some(notes) = update.notes.as_deref().filter(|notes| !notes.trim().is_empty()) {
        println!();
        for line in notes.lines() {
            println!("  {line}");
        }
        println!();
    }

    let plan = match update.plan() {
        updater::ApplyPlan::RunInstaller => "run the installer",
        updater::ApplyPlan::SwapExecutable => "replace this executable",
        updater::ApplyPlan::OpenArchive => "open the download",
    };
    println!("action     {plan}");

    if args.check {
        return Ok(());
    }

    // Downloads are staged outside the game directory so a stale artifact is never
    // picked up as game content.
    let staging = data.root().join("update");
    let downloaded = updater::download(&client, &update, &staging).await?;
    println!("saved      {}", downloaded.display());

    if args.download_only {
        return Ok(());
    }

    let current = std::env::current_exe().map_err(|err| {
        mango_core::MangoError::Other(format!("cannot locate this executable: {err}"))
    })?;

    // Applying runs or replaces programs, so it needs an explicit confirmation
    // unless the player asked for exactly that.
    if !confirm(&format!("Apply {} now?", update.version))? {
        println!("not applied");
        return Ok(());
    }

    let should_exit = updater::apply(&update, &downloaded, &current)?;
    if should_exit {
        println!("restart the launcher to use {}", update.version);
    }

    Ok(())
}

/// Ask before doing something the player cannot undo.
fn confirm(prompt: &str) -> mango_core::Result<bool> {
    use std::io::Write;

    print!("{prompt} [y/N] ");
    std::io::stdout()
        .flush()
        .map_err(|err| mango_core::MangoError::Other(format!("could not prompt: {err}")))?;

    let mut answer = String::new();
    std::io::stdin().read_line(&mut answer).map_err(|err| {
        mango_core::MangoError::Other(format!("could not read the answer: {err}"))
    })?;

    let answer = answer.trim().to_ascii_lowercase();
    Ok(answer == "y" || answer == "yes")
}
