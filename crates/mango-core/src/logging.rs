//! Tracing setup shared by the CLI and the desktop app.
//!
//! Logs go to stderr *and* to a daily rolling file so users can attach one when
//! reporting a launch failure.

use std::path::Path;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

use crate::error::Result;

/// Keeps the background log writer alive for the process lifetime.
#[derive(Debug)]
pub struct Logging {
    _guard: WorkerGuard,
}

/// Install the global subscriber. Safe to call twice; the second call is a no-op.
pub fn init(data_dir: &Path, verbose: bool) -> Result<Logging> {
    let logs_dir = data_dir.join("logs");
    std::fs::create_dir_all(&logs_dir)?;

    let appender = tracing_appender::rolling::daily(&logs_dir, "mango.log");
    let (file_writer, guard) = tracing_appender::non_blocking(appender);

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(if verbose { "debug" } else { "info" }));

    let subscriber = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(std::io::stderr).with_target(false))
        .with(fmt::layer().with_writer(file_writer).with_ansi(false).with_target(true));

    // A second launcher instance in the same process (tests) must not panic.
    let _ = subscriber.try_init();

    tracing::debug!(dir = %logs_dir.display(), "logging initialised");
    Ok(Logging { _guard: guard })
}
