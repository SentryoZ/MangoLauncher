//! MangoLauncher desktop app.

// A GUI binary gets no console window. Built as a console binary, launching the
// exe from Explorer would open a black cmd window behind the launcher. Log lines
// are collected in a sink and shown in the Logs screen instead.
#![cfg_attr(windows, windows_subsystem = "windows")]

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
            report_fatal(&format!("could not determine the data directory: {err}"));
            std::process::exit(1);
        }
    };

    if let Err(err) = data.ensure() {
        report_fatal(&format!("could not create {}: {err}", data.root().display()));
        std::process::exit(1);
    }

    // A GUI binary has no console, so log lines are collected for the Logs screen
    // instead of being written to a stderr nobody can see.
    let log_sink = mango_core::logging::LogSink::new();

    // Keep the guard alive for the process lifetime so buffered log lines flush.
    let _logging = mango_core::logging::init_with_sink(data.root(), false, log_sink.clone()).ok();

    let config = match Config::load(&data) {
        Ok(config) => config,
        Err(err) => {
            report_fatal(&format!("could not load configuration: {err}"));
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

    let env = AppEnv { data, game_root, config, dirty: false, log_sink };

    tracing::info!(version = mango_core::VERSION, "MangoLauncher starting");

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

/// Report a failure that happens before the window exists.
///
/// With no console attached there is no stderr to read, so a startup failure
/// would otherwise look like the launcher silently doing nothing.
fn report_fatal(message: &str) {
    tracing::error!("{message}");

    #[cfg(windows)]
    {
        // A plain message box is the only UI available this early.
        #[link(name = "user32")]
        unsafe extern "system" {
            fn MessageBoxW(
                hwnd: *mut std::ffi::c_void,
                text: *const u16,
                caption: *const u16,
                kind: u32,
            ) -> i32;
        }

        let wide =
            |text: &str| -> Vec<u16> { text.encode_utf16().chain(std::iter::once(0)).collect() };
        let text = wide(message);
        let caption = wide("MangoLauncher");

        const MB_ICONERROR: u32 = 0x0010;
        unsafe {
            MessageBoxW(std::ptr::null_mut(), text.as_ptr(), caption.as_ptr(), MB_ICONERROR);
        }
    }

    #[cfg(not(windows))]
    eprintln!("error: {message}");
}
