use std::time::Duration;

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
}

impl TerminalProgress {
    pub fn new(visible: bool) -> Self {
        let bar = if visible {
            ProgressBar::new_spinner()
        } else {
            ProgressBar::hidden()
        };
        if visible {
            bar.set_draw_target(ProgressDrawTarget::stderr());
            bar.set_style(spinner_style());
            bar.enable_steady_tick(Duration::from_millis(90));
        }
        Self { bar, visible }
    }
}

impl ProgressSink for TerminalProgress {
    fn emit(&self, event: ProgressEvent) {
        if !self.visible {
            return;
        }
        match event {
            ProgressEvent::LoadingBrowserCookies { browser } => {
                self.bar.set_style(spinner_style());
                self.bar.enable_steady_tick(Duration::from_millis(90));
                self.bar
                    .set_message(format!("Reading Pinterest cookies from {browser}"));
            }
            ProgressEvent::FetchingBoard => {
                self.bar.set_style(spinner_style());
                self.bar.enable_steady_tick(Duration::from_millis(90));
                self.bar.set_message("Fetching board metadata");
            }
            ProgressEvent::BoardResolved { name } => {
                self.bar.set_message(format!("Found board “{name}”"));
            }
            ProgressEvent::FetchingUserBoards { username } => {
                self.bar.set_style(spinner_style());
                self.bar.enable_steady_tick(Duration::from_millis(90));
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
            }
            ProgressEvent::SelectionFinished => {
                self.bar.set_draw_target(ProgressDrawTarget::stderr());
                self.bar.set_style(spinner_style());
                self.bar.enable_steady_tick(Duration::from_millis(90));
            }
            ProgressEvent::BoardStarted {
                name,
                current,
                total,
            } => {
                self.bar.set_style(spinner_style());
                self.bar.enable_steady_tick(Duration::from_millis(90));
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
                self.bar.set_style(spinner_style());
                self.bar.enable_steady_tick(Duration::from_millis(90));
                self.bar.set_message("Comparing image fingerprints");
            }
            ProgressEvent::ReportStarted => {
                self.bar.set_style(spinner_style());
                self.bar.enable_steady_tick(Duration::from_millis(90));
                self.bar.set_message("Creating temporary visual report");
            }
            ProgressEvent::Finished | ProgressEvent::Failed => {
                self.bar.disable_steady_tick();
                self.bar.finish_and_clear();
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
}
