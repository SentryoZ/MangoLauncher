use mango_core::Result;
use mango_core::config::Config;
use mango_core::paths::{DataPaths, GamePaths};

pub fn run(data: &DataPaths, game: &GamePaths, config: &Config) -> Result<()> {
    println!("MangoLauncher {}", mango_core::VERSION);
    println!();
    println!("data dir        {}", data.root().display());
    println!("config file     {}", data.config_file().display());
    println!("logs dir        {}", data.logs_dir().display());
    println!();
    println!("game dir        {}", game.root().display());
    println!("common          {}", game.common_dir().display());
    println!("instances       {}", game.instances_dir().display());
    println!("java            {}", game.java_dir().display());
    println!();
    println!("selected server {}", config.selected_server.as_deref().unwrap_or("<none>"));
    println!("selected account {}", config.selected_account.as_deref().unwrap_or("<none>"));
    println!("accounts         {}", config.accounts.len());
    println!(
        "resolution       {}x{} (fullscreen: {})",
        config.settings.game.res_width,
        config.settings.game.res_height,
        config.settings.game.fullscreen
    );
    Ok(())
}
