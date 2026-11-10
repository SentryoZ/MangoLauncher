//! Tracing setup shared by the CLI and the desktop app.
//!
//! Every log line goes to a daily rolling file so users can attach one when
//! reporting a launch failure. Where the *live* copy goes depends on the front
//! end: the CLI writes to stderr, while the desktop app keeps no console (a
//! Windows GUI binary has none) and collects the lines in a [`LogSink`] for its
//! Logs screen instead.

use std::collections::VecDeque;
use std::path::Path;
use std::sync::{Arc, Mutex};

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::prelude::*;
use tracing_subscriber::{EnvFilter, fmt};

use crate::error::Result;

/// How many lines the desktop log view may hold before the oldest are dropped.
const SINK_CAPACITY: usize = 5000;

/// Keeps the background log writer alive for the process lifetime.
#[derive(Debug)]
pub struct Logging {
    _guard: WorkerGuard,
}

/// A bounded, thread-safe buffer that a UI can drain into its log view.
///
/// The buffer is cheap to clone and is written from whatever thread produces a
/// log line, including the download and game-output tasks.
#[derive(Clone, Default)]
pub struct LogSink {
    lines: Arc<Mutex<VecDeque<String>>>,
    /// Called after a line is stored, so the UI can repaint without polling.
    notifier: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl std::fmt::Debug for LogSink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let pending = self.lines.lock().map(|lines| lines.len()).unwrap_or(0);
        f.debug_struct("LogSink").field("pending", &pending).finish_non_exhaustive()
    }
}

impl LogSink {
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach a callback that runs whenever a line arrives.
    ///
    /// Without it the buffer still works, but the UI would only show new lines
    /// on its next unrelated repaint.
    pub fn with_notifier(mut self, notify: impl Fn() + Send + Sync + 'static) -> Self {
        self.notifier = Some(Arc::new(notify));
        self
    }

    /// Store one line, dropping the oldest once the buffer is full.
    pub fn push(&self, line: String) {
        if line.trim().is_empty() {
            return;
        }

        if let Ok(mut lines) = self.lines.lock() {
            if lines.len() >= SINK_CAPACITY {
                lines.pop_front();
            }
            lines.push_back(line);
        }

        if let Some(notify) = &self.notifier {
            notify();
        }
    }

    /// Take everything buffered since the last call.
    pub fn drain(&self) -> Vec<String> {
        match self.lines.lock() {
            Ok(mut lines) => lines.drain(..).collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// Install the global subscriber with file logging only.
///
/// The second call is a no-op, so a test cannot panic on a duplicate init.
pub fn init(data_dir: &Path, verbose: bool) -> Result<Logging> {
    install(data_dir, verbose, None)
}

/// Install the global subscriber, collecting live lines into `sink`.
///
/// Used by the desktop app: it has no usable stderr, so the sink is the only way
/// the player can see what happened.
pub fn init_with_sink(data_dir: &Path, verbose: bool, sink: LogSink) -> Result<Logging> {
    install(data_dir, verbose, Some(sink))
}

fn install(data_dir: &Path, verbose: bool, sink: Option<LogSink>) -> Result<Logging> {
    let logs_dir = data_dir.join("logs");
    std::fs::create_dir_all(&logs_dir)?;

    let appender = tracing_appender::rolling::daily(&logs_dir, "mango.log");
    let (file_writer, guard) = tracing_appender::non_blocking(appender);

    let filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new(if verbose { "debug" } else { "info" }));

    let registry = tracing_subscriber::registry()
        .with(filter)
        .with(fmt::layer().with_writer(file_writer).with_ansi(false).with_target(true));

    // A sink replaces stderr rather than joining it: a GUI binary's stderr goes
    // nowhere, so writing there would only cost time.
    let _ = match sink {
        Some(sink) => registry
            .with(fmt::layer().with_writer(sink).with_ansi(false).with_target(true))
            .try_init(),
        None => {
            registry.with(fmt::layer().with_writer(std::io::stderr).with_target(false)).try_init()
        }
    };

    tracing::debug!(dir = %logs_dir.display(), "logging initialised");
    Ok(Logging { _guard: guard })
}

/// Adapter that turns the formatter's byte stream into whole lines.
pub struct SinkWriter {
    sink: LogSink,
    pending: String,
}

impl std::io::Write for SinkWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.pending.push_str(&String::from_utf8_lossy(bytes));

        // The formatter writes one record per call and ends it with a newline,
        // but splitting on newlines keeps a multi-line message together.
        while let Some(index) = self.pending.find('\n') {
            let line: String = self.pending.drain(..=index).collect();
            self.sink.push(line.trim_end().to_owned());
        }

        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        if !self.pending.trim().is_empty() {
            self.sink.push(self.pending.trim_end().to_owned());
        }
        self.pending.clear();
        Ok(())
    }
}

impl Drop for SinkWriter {
    fn drop(&mut self) {
        use std::io::Write;
        let _ = Write::flush(self);
    }
}

impl<'a> fmt::MakeWriter<'a> for LogSink {
    type Writer = SinkWriter;

    fn make_writer(&'a self) -> Self::Writer {
        SinkWriter { sink: self.clone(), pending: String::new() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sink_drains_once() {
        let sink = LogSink::new();
        sink.push("first".to_owned());
        sink.push("second".to_owned());

        assert_eq!(sink.drain(), vec!["first".to_owned(), "second".to_owned()]);
        assert!(sink.drain().is_empty(), "draining must empty the buffer");
    }

    #[test]
    fn blank_lines_are_not_stored() {
        let sink = LogSink::new();
        sink.push("  ".to_owned());
        sink.push(String::new());
        assert!(sink.drain().is_empty());
    }

    #[test]
    fn the_oldest_lines_are_dropped_when_full() {
        let sink = LogSink::new();
        for index in 0..SINK_CAPACITY + 10 {
            sink.push(format!("line {index}"));
        }

        let lines = sink.drain();
        assert_eq!(lines.len(), SINK_CAPACITY);
        assert_eq!(lines[0], "line 10", "the first ten should have been evicted");
    }

    #[test]
    fn the_notifier_fires_per_line() {
        let hits = Arc::new(Mutex::new(0));
        let counter = Arc::clone(&hits);
        let sink = LogSink::new().with_notifier(move || {
            if let Ok(mut hits) = counter.lock() {
                *hits += 1;
            }
        });

        sink.push("one".to_owned());
        sink.push("two".to_owned());

        assert_eq!(*hits.lock().unwrap(), 2);
    }

    #[test]
    fn the_writer_splits_a_message_into_lines() {
        use std::io::Write;
        use tracing_subscriber::fmt::MakeWriter;

        let sink = LogSink::new();
        let mut writer = sink.make_writer();
        writer.write_all(b"alpha\nbeta\n").unwrap();

        assert_eq!(sink.drain(), vec!["alpha".to_owned(), "beta".to_owned()]);
    }

    #[test]
    fn a_partial_line_is_flushed_on_drop() {
        use std::io::Write;
        use tracing_subscriber::fmt::MakeWriter;

        let sink = LogSink::new();
        {
            let mut writer = sink.make_writer();
            writer.write_all(b"no trailing newline").unwrap();
        }

        assert_eq!(sink.drain(), vec!["no trailing newline".to_owned()]);
    }

    /// The point of the sink: a line logged anywhere reaches the UI buffer.
    ///
    /// The global subscriber can only be installed once per process, so this test
    /// drives the formatter directly rather than through `init_with_sink`, which
    /// another test in the same binary may already have taken.
    #[test]
    fn a_formatted_record_lands_in_the_sink() {
        use std::io::Write;
        use tracing_subscriber::fmt::MakeWriter;

        let sink = LogSink::new();
        {
            let mut writer = sink.make_writer();
            writer.write_all(b"2026-11-10 INFO downloading assets\n").unwrap();
        }

        let lines = sink.drain();
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(lines[0].contains("downloading assets"), "{}", lines[0]);
    }
}
