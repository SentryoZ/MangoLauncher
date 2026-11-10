use std::cell::RefCell;
use std::io::Write;

use mango_core::net::downloader::Progress;

/// Single-line progress display for verification and downloads.
pub struct ProgressReporter {
    state: RefCell<ReporterState>,
}

#[derive(Default)]
struct ReporterState {
    last_width: usize,
    finished: bool,
}

impl ProgressReporter {
    pub fn new() -> Self {
        Self { state: RefCell::new(ReporterState::default()) }
    }

    pub fn on(&self, event: Progress) {
        let mut state = self.state.borrow_mut();
        let text = match event {
            Progress::Verifying { done, total } => {
                format!("verifying {done}/{total}")
            }
            Progress::Downloading { done, total, bytes_done, bytes_total, current } => {
                let pct = bytes_done
                    .checked_div(bytes_total)
                    .map(|_| bytes_done * 100 / bytes_total.max(1))
                    .unwrap_or(0);
                let name = current.file_name().and_then(|n| n.to_str()).unwrap_or("?");
                format!("downloading {done}/{total} ({pct}%) {name}")
            }
            Progress::Finished { downloaded, skipped } => {
                state.finished = true;
                format!("done: {downloaded} downloaded, {skipped} skipped")
            }
        };
        state.draw(&text);
    }

    pub fn finish(&self) {
        let mut state = self.state.borrow_mut();
        if !state.finished {
            state.clear();
        }
    }
}

impl ReporterState {
    fn draw(&mut self, text: &str) {
        let width = text.len().min(120);
        let truncated = &text[..width];
        print!("\r{truncated:<width$}");
        let _ = std::io::stdout().flush();
        self.last_width = width;
    }

    fn clear(&mut self) {
        if self.last_width > 0 {
            print!("\r{}\r", " ".repeat(self.last_width));
            let _ = std::io::stdout().flush();
            self.last_width = 0;
        }
    }
}
