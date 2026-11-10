//! MangoLauncher desktop app.

mod app;
mod runtime;
mod state;
mod theme;
mod views;

use std::path::PathBuf;

use mango_core::config::Config;
use mango_core::paths::DataPaths;

use app::{AppEnv, MangoApp};

fn main() -> eframe::Result<()> {
    let data = match DataPaths::from_env() {
        Ok(paths) => paths,
        Err(err) => {
            eprintln!("error: could not determine the data directory: {err}");
            std::process::exit(1);
        }
    };

    if let Err(err) = data.ensure() {
        eprintln!("error: could not create {}: {err}", data.root().display());
        std::process::exit(1);
    }

    // Keep the guard alive for the process lifetime so buffered log lines flush.
    let _logging = mango_core::logging::init(data.root(), false).ok();

    let config = match Config::load(&data) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("error: could not load configuration: {err}");
            std::process::exit(1);
        }
    };

    let game_root = config
        .settings
        .launcher
        .data_directory
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| data.root().to_path_buf());

    let env = AppEnv { data, game_root, config, dirty: false };

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([980.0, 560.0])
            .with_min_inner_size([820.0, 480.0])
            .with_title("MangoLauncher"),
        ..Default::default()
    };

    eframe::run_native(
        "MangoLauncher",
        options,
        Box::new(|cc| Ok(Box::new(MangoApp::new(cc, env)))),
    )
}
