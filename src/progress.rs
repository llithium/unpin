use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use console::Term;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ProgressEvent {
    LoadingBrowserCookies {
        browser: String,
    },
    FetchingBoard,
    BoardResolved {
        name: String,
    },
    FetchingUserBoards {
        username: String,
    },
    UserBoardsResolved {
        total: usize,
    },
    /// The board picker is about to take over the terminal.
    SelectionStarted,
    SelectionFinished,
    BoardStarted {
        name: String,
        current: usize,
        total: usize,
    },
    PageFetched {
        resource: &'static str,
        page: usize,
        items: usize,
    },
    SectionsStarted {
        total: usize,
    },
    SectionStarted {
        current: usize,
        total: usize,
    },
    ImagesStarted {
        total: usize,
    },
    ImageFinished {
        completed: usize,
        total: usize,
    },
    MatchingStarted,
    ReportStarted,
    Finished,
    Failed,
}

pub trait ProgressSink: Send + Sync {
    fn emit(&self, event: ProgressEvent);
}

#[derive(Debug, Default)]
pub struct NoProgress;

impl ProgressSink for NoProgress {
    fn emit(&self, _event: ProgressEvent) {}
}

#[derive(Debug)]
pub struct TerminalProgress {
    bar: ProgressBar,
    visible: bool,
    cursor_hidden: AtomicBool,
}

impl TerminalProgress {
    pub fn new(visible: bool) -> Self {
        let bar = if visible {
            ProgressBar::new_spinner()
        } else {
            ProgressBar::hidden()
        };
        let progress = Self {
            bar,
            visible,
            cursor_hidden: AtomicBool::new(false),
        };
        if visible {
            progress.bar.set_draw_target(ProgressDrawTarget::stderr());
            progress.resume_spinner();
            // indicatif leaves the caret parked at the end of the bar, where it
            // blinks over the output for the whole scan.
            progress.hide_cursor();
        }
        progress
    }

    /// Restores the spinner style and tick after the bar switched away from it.
    fn resume_spinner(&self) {
        self.bar.set_style(spinner_style());
        self.bar.enable_steady_tick(Duration::from_millis(90));
    }

    fn hide_cursor(&self) {
        if self.visible && !self.cursor_hidden.swap(true, Ordering::Relaxed) {
            let _ = Term::stderr().hide_cursor();
        }
    }

    /// Always safe to call, and called from every path that ends the bar.
    fn show_cursor(&self) {
        if self.cursor_hidden.swap(false, Ordering::Relaxed) {
            let _ = Term::stderr().show_cursor();
        }
    }
}

impl Drop for TerminalProgress {
    /// Backstop for early returns and panics; a terminal left without a caret
    /// needs a manual `reset` to recover.
    fn drop(&mut self) {
        self.show_cursor();
    }
}

/// Restores the caret when the process is interrupted, which skips `Drop`.
///
/// A failed install is not worth reporting: the worst case is the pre-existing
/// behavior of an interrupted run leaving the caret hidden.
pub fn restore_cursor_on_interrupt() {
    let _ = ctrlc::set_handler(|| {
        let _ = Term::stderr().show_cursor();
        // 128 + SIGINT, the shell convention for death by interrupt.
        std::process::exit(130);
    });
}

impl ProgressSink for TerminalProgress {
    fn emit(&self, event: ProgressEvent) {
        if !self.visible {
            return;
        }
        match event {
            ProgressEvent::LoadingBrowserCookies { browser } => {
                self.resume_spinner();
                self.bar
                    .set_message(format!("Reading Pinterest cookies from {browser}"));
            }
            ProgressEvent::FetchingBoard => {
                self.resume_spinner();
                self.bar.set_message("Fetching board metadata");
            }
            ProgressEvent::BoardResolved { name } => {
                self.bar.set_message(format!("Found board “{name}”"));
            }
            ProgressEvent::FetchingUserBoards { username } => {
                self.resume_spinner();
                self.bar
                    .set_message(format!("Listing boards for {username}"));
            }
            ProgressEvent::UserBoardsResolved { total } => {
                self.bar.set_message(format!("Found {total} board(s)"));
            }
            // Hide rather than finish the bar: a finished bar never redraws,
            // and the scan continues after the picker closes.
            ProgressEvent::SelectionStarted => {
                self.bar.disable_steady_tick();
                self.bar.set_draw_target(ProgressDrawTarget::hidden());
                self.show_cursor();
            }
            ProgressEvent::SelectionFinished => {
                self.hide_cursor();
                self.bar.set_draw_target(ProgressDrawTarget::stderr());
                self.resume_spinner();
            }
            ProgressEvent::BoardStarted {
                name,
                current,
                total,
            } => {
                self.resume_spinner();
                self.bar
                    .set_message(format!("Board {current}/{total}: “{name}”"));
            }
            ProgressEvent::PageFetched {
                resource,
                page,
                items,
            } => {
                self.bar
                    .set_message(format!("Fetching {resource}: page {page}, {items} item(s)"));
            }
            ProgressEvent::SectionsStarted { total } => {
                self.bar
                    .set_message(format!("Fetching {total} board section(s)"));
            }
            ProgressEvent::SectionStarted { current, total } => {
                self.bar
                    .set_message(format!("Fetching board section {current}/{total}"));
            }
            ProgressEvent::ImagesStarted { total } => {
                self.bar.disable_steady_tick();
                self.bar.set_length(total as u64);
                self.bar.set_position(0);
                self.bar.set_style(download_style());
                self.bar.set_message("Analyzing images");
            }
            ProgressEvent::ImageFinished { completed, total } => {
                self.bar.set_length(total as u64);
                self.bar.set_position(completed as u64);
            }
            ProgressEvent::MatchingStarted => {
                self.resume_spinner();
                self.bar.set_message("Comparing image fingerprints");
            }
            ProgressEvent::ReportStarted => {
                self.resume_spinner();
                self.bar.set_message("Creating temporary visual report");
            }
            ProgressEvent::Finished | ProgressEvent::Failed => {
                self.bar.disable_steady_tick();
                self.bar.finish_and_clear();
                self.show_cursor();
            }
        }
    }
}

fn spinner_style() -> ProgressStyle {
    ProgressStyle::with_template("{spinner:.cyan} {msg}")
        .expect("the static spinner template is valid")
        .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
}

fn download_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "{spinner:.cyan} {msg} [{bar:32.cyan/blue}] {pos}/{len} ({elapsed_precise})",
    )
    .expect("the static progress template is valid")
    .progress_chars("━━╾")
}

#[cfg(test)]
pub mod tests {
    use std::sync::Mutex;

    use super::*;

    #[derive(Debug, Default)]
    pub struct RecordingProgress {
        events: Mutex<Vec<ProgressEvent>>,
    }

    impl RecordingProgress {
        pub fn events(&self) -> Vec<ProgressEvent> {
            self.events.lock().unwrap().clone()
        }
    }

    impl ProgressSink for RecordingProgress {
        fn emit(&self, event: ProgressEvent) {
            self.events.lock().unwrap().push(event);
        }
    }

    #[test]
    fn non_interactive_runs_never_touch_the_cursor() {
        // Piped or --no-progress runs must not emit terminal escapes at all.
        let progress = TerminalProgress::new(false);
        for event in [
            ProgressEvent::FetchingBoard,
            ProgressEvent::SelectionStarted,
            ProgressEvent::SelectionFinished,
            ProgressEvent::Finished,
        ] {
            progress.emit(event);
            assert!(!progress.cursor_hidden.load(Ordering::Relaxed));
        }
    }

    #[test]
    fn finishing_restores_a_hidden_cursor() {
        let progress = TerminalProgress::new(false);
        // Force the hidden state a visible run would be in.
        progress.cursor_hidden.store(true, Ordering::Relaxed);

        progress.show_cursor();
        assert!(!progress.cursor_hidden.load(Ordering::Relaxed));
        // Restoring twice is harmless, which is what makes Drop a safe backstop.
        progress.show_cursor();
        assert!(!progress.cursor_hidden.load(Ordering::Relaxed));
    }
}
