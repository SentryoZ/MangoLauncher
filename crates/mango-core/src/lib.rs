//! Core library for MangoLauncher.
//!
//! Everything game- or launcher-related lives here so that the CLI and the
//! desktop UI share exactly the same behaviour. The UI crate must never talk
//! to the network or the filesystem directly.

pub mod auth;
pub mod config;
pub mod error;
pub mod i18n;
pub mod install;
pub mod instance;
pub mod java;
pub mod launch;
pub mod loaders;
pub mod logging;
pub mod manifest;
pub mod mods;
pub mod mojang;
pub mod net;
pub mod paths;
pub mod server;

pub use error::{MangoError, Result};

/// Launcher version, used in user agents and launch arguments.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Value passed to Minecraft as `launcher_name`.
pub const LAUNCHER_NAME: &str = "MangoLauncher";
