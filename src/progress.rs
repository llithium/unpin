use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use console::Term;
use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};

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
    /// `current` is the deterministic launch ordinal; completion is reported
    /// separately because several boards may be in flight at once.
    BoardStarted {
        name: String,
        current: usize,
        total: usize,
    },
    BoardFinished {
        name: String,
        completed: usize,
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
    /// `current` is the section's launch ordinal within its board.
    SectionStarted {
        current: usize,
        total: usize,
    },
    SectionFinished {
        completed: usize,
        total: usize,
    },
    RequestRetry {
        resource: &'static str,
        attempt: usize,
        delay: Duration,
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
    bars: MultiProgress,
    visible: bool,
    cursor_hidden: AtomicBool,
    state: Mutex<ProgressState>,
}

#[derive(Debug, Default)]
struct ProgressState {
    groups: HashSet<ProgressGroup>,
    setup: Option<ProgressBar>,
    page: Option<ProgressBar>,
    page_resource: Option<&'static str>,
    sections: Option<ProgressBar>,
    images: Option<ProgressBar>,
    matching: Option<ProgressBar>,
    report: Option<ProgressBar>,
    board_rows: Vec<BoardRow>,
    boards_total: usize,
    boards_completed: usize,
    sections_total: usize,
    sections_started: usize,
    sections_completed: usize,
}

#[derive(Debug)]
struct BoardRow {
    name: String,
    bar: ProgressBar,
}

#[derive(Debug, Clone, Copy, Eq, Hash, PartialEq)]
enum ProgressGroup {
    Setup,
    Boards,
    Analysis,
    Report,
    Complete,
}

impl ProgressGroup {
    fn label(self) -> &'static str {
        match self {
            Self::Setup => "> Setup",
            Self::Boards => "> Boards",
            Self::Analysis => "> Analysis",
            Self::Report => "> Report",
            Self::Complete => "> Complete",
        }
    }
}

impl TerminalProgress {
    pub fn new(visible: bool) -> Self {
        let bars = if visible {
            MultiProgress::with_draw_target(ProgressDrawTarget::stderr())
        } else {
            MultiProgress::with_draw_target(ProgressDrawTarget::hidden())
        };
        let progress = Self {
            bars,
            visible,
            cursor_hidden: AtomicBool::new(false),
            state: Mutex::new(ProgressState::default()),
        };
        if visible {
            progress.bars.set_move_cursor(false);
            progress.add_header("unpin");
            // indicatif leaves the caret parked at the end of the bar, where it
            // it blinks over the checklist for the whole scan.
            progress.hide_cursor();
        }
        progress
    }

    fn add_header(&self, message: &str) {
        let bar = self.bars.add(ProgressBar::new_spinner());
        bar.set_style(header_style());
        bar.set_message(message.to_owned());
        bar.finish();
    }

    fn add_group(&self, state: &mut ProgressState, group: ProgressGroup) {
        if state.groups.insert(group) {
            let bar = self.bars.add(ProgressBar::new_spinner());
            bar.set_style(group_style());
            bar.set_message(group.label().to_owned());
            bar.finish();
        }
    }

    fn add_active_row(&self, message: String) -> ProgressBar {
        let bar = self.bars.add(ProgressBar::new_spinner());
        bar.set_style(active_style());
        bar.set_message(message);
        bar.enable_steady_tick(Duration::from_millis(90));
        bar
    }

    fn complete_row(bar: &ProgressBar, message: String) {
        bar.disable_steady_tick();
        bar.set_style(completed_style());
        bar.finish_with_message(message);
    }

    fn fail_row(bar: &ProgressBar, message: String) {
        bar.disable_steady_tick();
        bar.set_style(failed_style());
        bar.finish_with_message(message);
    }

    fn finish_slot(slot: &mut Option<ProgressBar>, message: String) {
        if let Some(bar) = slot.take() {
            Self::complete_row(&bar, message);
        }
    }

    fn fail_slot(slot: &mut Option<ProgressBar>, message: String) {
        if let Some(bar) = slot.take() {
            Self::fail_row(&bar, message);
        }
    }

    fn ensure_active_slot(&self, slot: &mut Option<ProgressBar>, message: String) -> ProgressBar {
        if let Some(bar) = slot {
            bar.set_message(message);
            bar.clone()
        } else {
            let bar = self.add_active_row(message);
            *slot = Some(bar.clone());
            bar
        }
    }

    fn finish_active_rows(&self, state: &mut ProgressState) {
        Self::finish_slot(&mut state.setup, "Setup complete".into());
        Self::finish_slot(&mut state.page, "Pins fetched".into());
        Self::finish_slot(&mut state.sections, "Board sections fetched".into());
        Self::finish_slot(&mut state.images, "Images analyzed".into());
        Self::finish_slot(&mut state.matching, "Matches compared".into());
        Self::finish_slot(&mut state.report, "Report created".into());
        for row in &state.board_rows {
            if !row.bar.is_finished() {
                Self::complete_row(&row.bar, format!("Scanned board “{}”", row.name));
            }
        }
    }

    fn fail_active_rows(&self, state: &mut ProgressState) {
        Self::fail_slot(&mut state.setup, "Setup failed".into());
        Self::fail_slot(&mut state.page, "Fetching pins failed".into());
        Self::fail_slot(&mut state.sections, "Fetching board sections failed".into());
        Self::fail_slot(&mut state.images, "Image analysis failed".into());
        Self::fail_slot(&mut state.matching, "Matching failed".into());
        Self::fail_slot(&mut state.report, "Report failed".into());
        for row in &state.board_rows {
            if !row.bar.is_finished() {
                Self::fail_row(&row.bar, format!("Scanning board “{}” failed", row.name));
            }
        }
    }

    fn redraw_active_rows(&self, state: &ProgressState) {
        let mut bars = Vec::new();
        for bar in [
            state.setup.as_ref(),
            state.page.as_ref(),
            state.sections.as_ref(),
            state.images.as_ref(),
            state.matching.as_ref(),
            state.report.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            bars.push(bar.clone());
        }
        bars.extend(
            state
                .board_rows
                .iter()
                .filter(|row| !row.bar.is_finished())
                .map(|row| row.bar.clone()),
        );
        for bar in bars {
            bar.tick();
        }
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
                let mut state = self.state.lock().unwrap();
                self.add_group(&mut state, ProgressGroup::Setup);
                Self::finish_slot(&mut state.setup, "Setup ready".into());
                state.setup =
                    Some(self.add_active_row(format!("Reading Pinterest cookies from {browser}")));
            }
            ProgressEvent::FetchingBoard => {
                let mut state = self.state.lock().unwrap();
                self.add_group(&mut state, ProgressGroup::Setup);
                Self::finish_slot(&mut state.setup, "Pinterest session ready".into());
                state.setup = Some(self.add_active_row("Fetching board metadata".into()));
            }
            ProgressEvent::BoardResolved { name } => {
                let mut state = self.state.lock().unwrap();
                Self::finish_slot(&mut state.setup, format!("Found board “{name}”"));
            }
            ProgressEvent::FetchingUserBoards { username } => {
                let mut state = self.state.lock().unwrap();
                self.add_group(&mut state, ProgressGroup::Setup);
                Self::finish_slot(&mut state.setup, "Pinterest session ready".into());
                state.setup = Some(self.add_active_row(format!("Listing boards for {username}")));
            }
            ProgressEvent::UserBoardsResolved { total } => {
                let mut state = self.state.lock().unwrap();
                Self::finish_slot(&mut state.setup, format!("Found {total} board(s)"));
            }
            // Hide rather than finish the bar: a finished bar never redraws,
            // and the scan continues after the picker closes.
            ProgressEvent::SelectionStarted => {
                self.bars.set_draw_target(ProgressDrawTarget::hidden());
                self.show_cursor();
            }
            ProgressEvent::SelectionFinished => {
                self.hide_cursor();
                self.bars.set_draw_target(ProgressDrawTarget::stderr());
                let state = self.state.lock().unwrap();
                self.redraw_active_rows(&state);
            }
            ProgressEvent::BoardStarted {
                name,
                current,
                total,
            } => {
                let mut state = self.state.lock().unwrap();
                self.add_group(&mut state, ProgressGroup::Boards);
                if state.boards_total != total {
                    state.boards_total = total;
                    state.boards_completed = 0;
                }
                let bar =
                    self.add_active_row(format!("Scanning board “{name}” ({current}/{total})"));
                state.board_rows.push(BoardRow { name, bar });
            }
            ProgressEvent::BoardFinished {
                name,
                completed,
                total,
            } => {
                let mut state = self.state.lock().unwrap();
                state.boards_total = total;
                // Completion events carry an atomic snapshot, but a task may
                // be paused between taking it and emitting it.
                state.boards_completed = state.boards_completed.max(completed);
                if let Some(row) = state
                    .board_rows
                    .iter()
                    .find(|row| row.name == name && !row.bar.is_finished())
                {
                    Self::complete_row(&row.bar, format!("Scanned board “{name}”"));
                }
            }
            ProgressEvent::PageFetched {
                resource,
                page,
                items,
            } => {
                let mut state = self.state.lock().unwrap();
                self.add_group(&mut state, ProgressGroup::Boards);
                if state.page_resource != Some(resource) {
                    if let Some(previous) = state.page_resource {
                        Self::finish_slot(&mut state.page, format!("Fetched {previous}"));
                    }
                    state.page_resource = Some(resource);
                }
                let message = format!("Fetching {resource}: page {page} · {items} item(s)");
                self.ensure_active_slot(&mut state.page, message);
            }
            ProgressEvent::SectionsStarted { total } => {
                let mut state = self.state.lock().unwrap();
                self.add_group(&mut state, ProgressGroup::Boards);
                state.sections_total += total;
                let message = format!(
                    "Fetching board sections: {}/{} complete",
                    state.sections_completed, state.sections_total
                );
                self.ensure_active_slot(&mut state.sections, message);
                if total == 0 {
                    Self::finish_slot(&mut state.sections, "No board sections found".into());
                }
            }
            ProgressEvent::SectionStarted { total, .. } => {
                let mut state = self.state.lock().unwrap();
                self.add_group(&mut state, ProgressGroup::Boards);
                state.sections_started += 1;
                state.sections_total = state.sections_total.max(total);
                let active = state
                    .sections_started
                    .saturating_sub(state.sections_completed);
                let message = format!(
                    "Fetching board sections: {}/{} complete ({active} active)",
                    state.sections_completed, state.sections_total
                );
                self.ensure_active_slot(&mut state.sections, message);
            }
            ProgressEvent::SectionFinished {
                completed: _,
                total,
            } => {
                let mut state = self.state.lock().unwrap();
                state.sections_total = state.sections_total.max(total);
                state.sections_completed += 1;
                let active = state
                    .sections_started
                    .saturating_sub(state.sections_completed);
                let message = format!(
                    "Fetching board sections: {}/{} complete ({active} active)",
                    state.sections_completed, state.sections_total
                );
                if let Some(bar) = state.sections.as_ref() {
                    bar.set_message(message);
                }
                if state.sections_completed >= state.sections_total {
                    Self::finish_slot(&mut state.sections, "Board sections fetched".into());
                }
            }
            ProgressEvent::RequestRetry {
                resource,
                attempt,
                delay,
            } => {
                let mut state = self.state.lock().unwrap();
                self.add_group(&mut state, ProgressGroup::Boards);
                if state.page_resource != Some(resource) {
                    if let Some(previous) = state.page_resource {
                        Self::finish_slot(&mut state.page, format!("Fetched {previous}"));
                    }
                    state.page_resource = Some(resource);
                }
                let message = format!(
                    "Retrying Pinterest {resource} request (attempt {attempt}) in {:.1}s",
                    delay.as_secs_f64()
                );
                self.ensure_active_slot(&mut state.page, message);
            }
            ProgressEvent::ImagesStarted { total } => {
                let mut state = self.state.lock().unwrap();
                self.add_group(&mut state, ProgressGroup::Analysis);
                Self::finish_slot(&mut state.page, "Pins fetched".into());
                Self::finish_slot(&mut state.sections, "Board sections fetched".into());
                let message = format!("Analyzing images (0/{total} complete)");
                let bar = self.ensure_active_slot(&mut state.images, message);
                bar.set_length(total as u64);
                bar.set_position(0);
                if total == 0 {
                    Self::finish_slot(&mut state.images, "Images analyzed (0/0)".into());
                }
            }
            ProgressEvent::ImageFinished { completed, total } => {
                let mut state = self.state.lock().unwrap();
                if let Some(bar) = state.images.as_ref() {
                    bar.set_length(total as u64);
                    bar.set_position(completed as u64);
                    bar.set_message(format!("Analyzing images ({completed}/{total} complete)"));
                }
                if completed >= total {
                    Self::finish_slot(
                        &mut state.images,
                        format!("Analyzed images ({completed}/{total})"),
                    );
                }
            }
            ProgressEvent::MatchingStarted => {
                let mut state = self.state.lock().unwrap();
                self.add_group(&mut state, ProgressGroup::Analysis);
                Self::finish_slot(&mut state.images, "Images analyzed".into());
                state.matching = Some(self.add_active_row("Comparing image fingerprints".into()));
            }
            ProgressEvent::ReportStarted => {
                let mut state = self.state.lock().unwrap();
                self.add_group(&mut state, ProgressGroup::Report);
                Self::finish_slot(&mut state.matching, "Matches compared".into());
                state.report = Some(self.add_active_row("Creating temporary visual report".into()));
            }
            ProgressEvent::Finished => {
                let mut state = self.state.lock().unwrap();
                self.finish_active_rows(&mut state);
                self.add_group(&mut state, ProgressGroup::Complete);
                let bar = self.add_active_row("Scan complete".into());
                Self::complete_row(&bar, "Scan complete".into());
                self.show_cursor();
            }
            ProgressEvent::Failed => {
                let mut state = self.state.lock().unwrap();
                self.fail_active_rows(&mut state);
                self.add_group(&mut state, ProgressGroup::Complete);
                let bar = self.add_active_row("Scan failed".into());
                Self::fail_row(&bar, "Scan failed".into());
                self.show_cursor();
            }
        }
    }
}

fn header_style() -> ProgressStyle {
    ProgressStyle::with_template("{msg:.bold}").expect("the static header template is valid")
}

fn group_style() -> ProgressStyle {
    ProgressStyle::with_template("{msg:.magenta}").expect("the static group template is valid")
}

fn active_style() -> ProgressStyle {
    ProgressStyle::with_template("  {spinner:.cyan} {msg}")
        .expect("the static spinner template is valid")
        .tick_strings(&["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"])
}

fn completed_style() -> ProgressStyle {
    ProgressStyle::with_template("  {spinner:.green} {msg}")
        .expect("the static completed template is valid")
        .tick_strings(&["✓", "✓"])
}

fn failed_style() -> ProgressStyle {
    ProgressStyle::with_template("  {spinner:.red} {msg}")
        .expect("the static failed template is valid")
        .tick_strings(&["!", "!"])
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

    #[test]
    fn checklist_completion_uses_a_checkmark_and_failures_use_an_exclamation() {
        assert_eq!(completed_style().get_final_tick_str(), "✓");
        assert_eq!(failed_style().get_final_tick_str(), "!");
        assert_ne!(active_style().get_tick_str(0), "✓");
    }
}
