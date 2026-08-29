use std::collections::BTreeMap;
use std::fmt::Write;

use console::Style;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::pinterest::SkippedPin;
use crate::terminal_text::sanitize_terminal_text;

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Recommendation {
    Keep,
    Tie,
    DeleteCandidate,
}

impl Recommendation {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Keep => "KEEP",
            Self::Tie => "TIE",
            Self::DeleteCandidate => "DELETE?",
        }
    }

    pub(crate) fn css_class(&self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Tie => "tie",
            Self::DeleteCandidate => "delete",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ReportItem {
    pub pin_id: String,
    pub pin_url: String,
    /// Board this pin lives in; shown only when several boards were scanned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub board: Option<String>,
    pub image_url: String,
    pub width: u32,
    pub height: u32,
    pub byte_size: u64,
    pub recommendation: Recommendation,
}

/// Whether a match sits inside one board or spans several.
///
/// The difference matters when deciding what to delete: the same image saved
/// twice into one board is a redundant double-save, while the same image in two
/// boards is often deliberate.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum MatchScope {
    SameBoard,
    CrossBoard,
}

impl MatchScope {
    /// Classifies a match by the boards its pins came from.
    ///
    /// Items without a board—every item in a single-board scan—count as being
    /// on the same board, so the quiet case is the default.
    pub(crate) fn of(items: &[ReportItem]) -> Self {
        let mut boards = items.iter().filter_map(|item| item.board.as_deref());
        let Some(first) = boards.next() else {
            return Self::SameBoard;
        };
        if boards.any(|board| board != first) {
            Self::CrossBoard
        } else {
            Self::SameBoard
        }
    }

    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::SameBoard => "SAME BOARD",
            Self::CrossBoard => "ACROSS BOARDS",
        }
    }

    pub(crate) fn html_label(&self) -> &'static str {
        match self {
            Self::SameBoard => "Same board",
            Self::CrossBoard => "Across boards",
        }
    }

    pub(crate) fn css_class(&self) -> &'static str {
        match self {
            Self::SameBoard => "same-board",
            Self::CrossBoard => "cross-board",
        }
    }

    /// Keeps duplicates saved within one board at the front of the review
    /// queue, where they are the safest cleanup candidates.
    pub(crate) fn sort_priority(self) -> u8 {
        match self {
            Self::SameBoard => 0,
            Self::CrossBoard => 1,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct DuplicateGroup {
    pub scope: MatchScope,
    pub items: Vec<ReportItem>,
}

impl DuplicateGroup {
    /// Stable identity for review state. Item order is deliberately ignored so
    /// a ranking change cannot make an otherwise unchanged group look new.
    pub(crate) fn review_key(&self) -> String {
        match_review_key("exact", self.scope, &self.items)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct VisualCandidate {
    pub hash_distance: u8,
    pub similarity_percent: u8,
    pub scope: MatchScope,
    pub items: [ReportItem; 2],
}

impl VisualCandidate {
    /// Stable identity for review state. A candidate is the same review item
    /// when its two pin members and board scope are unchanged.
    pub(crate) fn review_key(&self) -> String {
        match_review_key("visual", self.scope, &self.items)
    }
}

/// One board that contributed pins to this report.
#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ScannedBoard {
    pub name: String,
    pub url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pins_reported: Option<usize>,
    pub pins_found: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct Summary {
    /// Set when the scan started from a profile rather than a single board.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    pub boards: Vec<ScannedBoard>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pins_reported: Option<usize>,
    pub pins_found: usize,
    pub analyzed: usize,
    pub skipped: usize,
    pub exact_groups: usize,
    pub visual_candidates: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct Report {
    pub summary: Summary,
    pub exact_groups: Vec<DuplicateGroup>,
    pub visual_candidates: Vec<VisualCandidate>,
    pub skipped: Vec<SkippedPin>,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visual_report: Option<String>,
}

impl Report {
    /// Title shared by the text and HTML renderers.
    ///
    /// A single board keeps its own name; several boards are titled by the
    /// profile they came from.
    pub fn title(&self) -> String {
        match self.summary.boards.as_slice() {
            [board] => board.name.clone(),
            boards => {
                let count = format!("{} boards", boards.len());
                match &self.summary.username {
                    Some(username) => format!("{username} {count}"),
                    None => count,
                }
            }
        }
    }

    /// Stable browser-storage namespace for this set of scan sources.
    ///
    /// The temporary HTML filename changes on every run, so it cannot be used
    /// as the namespace if review marks are meant to survive a new report.
    pub(crate) fn review_storage_key(&self) -> String {
        let mut sources = self
            .summary
            .boards
            .iter()
            .map(|board| (board.name.as_str(), board.url.as_str()))
            .collect::<Vec<_>>();
        sources.sort_unstable();

        let mut canonical = String::from("unpin-review-v2\0");
        if let Some(username) = &self.summary.username {
            canonical.push_str(username);
        }
        canonical.push('\0');
        for (name, url) in sources {
            canonical.push_str(name);
            canonical.push('\0');
            canonical.push_str(url);
            canonical.push('\0');
        }

        hex::encode(Sha256::digest(canonical.as_bytes()))
    }

    /// Board labels and scope tags are noise when everything came from the same
    /// board, so both renderers gate on this.
    pub(crate) fn shows_board_labels(&self) -> bool {
        self.summary.boards.len() > 1
    }

    /// Renders the scope tag appended to a match heading.
    ///
    /// Empty for a single-board scan, where every match is same-board by
    /// definition and the tag would say nothing.
    fn scope_suffix(&self, scope: MatchScope, theme: &TextTheme) -> String {
        if !self.shows_board_labels() {
            return String::new();
        }
        let tag = match scope {
            // Yellow is already the report's "a human must decide" color.
            MatchScope::CrossBoard => theme.warning(scope.label()),
            MatchScope::SameBoard => theme.dim(scope.label()),
        };
        format!(" {} {tag}", theme.dim("·"))
    }

    /// Counts matches by scope as `(same board, across boards)`, over exact
    /// groups and visual candidates together.
    pub fn scope_counts(&self) -> (usize, usize) {
        let scopes = self.exact_groups.iter().map(|group| group.scope).chain(
            self.visual_candidates
                .iter()
                .map(|candidate| candidate.scope),
        );

        let mut same = 0;
        let mut cross = 0;
        for scope in scopes {
            match scope {
                MatchScope::SameBoard => same += 1,
                MatchScope::CrossBoard => cross += 1,
            }
        }
        (same, cross)
    }

    fn cross_board_matches(&self) -> usize {
        self.scope_counts().1
    }

    pub fn render_text(&self) -> String {
        self.render_text_with_color(false)
    }

    pub fn render_text_with_color(&self, color: bool) -> String {
        let mut output = String::new();
        let theme = TextTheme { color };
        let summary = &self.summary;
        let _ = writeln!(
            output,
            "{}  {}",
            theme.heading("UNPIN"),
            theme.strong(self.title())
        );
        let _ = writeln!(output, "{}", theme.dim("Pinterest duplicate review"));
        let _ = writeln!(output);
        let _ = writeln!(
            output,
            "{}  {} returned{}  {} analyzed  {} skipped",
            theme.label("PINS"),
            theme.strong(summary.pins_found),
            summary
                .pins_reported
                .map(|reported| format!(" / {reported} reported"))
                .unwrap_or_default(),
            theme.success(summary.analyzed),
            theme.warning(summary.skipped)
        );
        let cross_board = self.cross_board_matches();
        let _ = writeln!(
            output,
            "{}  {} exact group(s)  {} visual candidate(s){}",
            theme.label("MATCHES"),
            theme.strong(summary.exact_groups),
            theme.accent(summary.visual_candidates),
            if self.shows_board_labels() && cross_board > 0 {
                format!(
                    "  {} {}",
                    theme.dim("·"),
                    theme.warning(format!("{cross_board} across boards"))
                )
            } else {
                String::new()
            }
        );

        if self.shows_board_labels() {
            let _ = writeln!(output, "\n{}", theme.label("BOARDS"));
            for board in &summary.boards {
                let _ = writeln!(
                    output,
                    "  {:>5}  {}",
                    theme.strong(board.pins_found),
                    sanitize_terminal_text(&board.name)
                );
            }
        }

        if self.exact_groups.is_empty() && self.visual_candidates.is_empty() {
            let _ = writeln!(output, "\n{}", theme.dim("No duplicate visual pins found."));
        }

        for (index, group) in self.exact_groups.iter().enumerate() {
            let _ = writeln!(
                output,
                "\n{}  {}{}",
                theme.section(format!("EXACT {:02}", index + 1)),
                theme.dim("byte-identical files"),
                self.scope_suffix(group.scope, &theme)
            );
            render_items(&mut output, &group.items, &theme, self.shows_board_labels());
        }

        for (index, candidate) in self.visual_candidates.iter().enumerate() {
            let _ = writeln!(
                output,
                "\n{}  {}{}",
                theme.section(format!("VISUAL {:02}", index + 1)),
                theme.dim(format!(
                    "{}% similarity  •  hash distance {}/64",
                    candidate.similarity_percent, candidate.hash_distance
                )),
                self.scope_suffix(candidate.scope, &theme)
            );
            render_items(
                &mut output,
                &candidate.items,
                &theme,
                self.shows_board_labels(),
            );
        }

        if !self.skipped.is_empty() {
            let _ = writeln!(
                output,
                "\n{}  {}",
                theme.warning("SKIPPED"),
                theme.dim(format!("{} pin(s)", self.skipped.len()))
            );
            let mut reasons = BTreeMap::new();
            for skipped in &self.skipped {
                *reasons.entry(skipped.reason.as_str()).or_insert(0_usize) += 1;
            }
            let mut reasons = reasons.into_iter().collect::<Vec<_>>();
            reasons.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
            for (reason, count) in reasons {
                let _ = writeln!(
                    output,
                    "  {}  {}",
                    theme.warning(format!("{count:>3}×")),
                    sanitize_terminal_text(reason)
                );
            }

            if self.skipped.len() <= 12 {
                for skipped in &self.skipped {
                    if let (Some(id), Some(url)) = (&skipped.pin_id, &skipped.pin_url) {
                        let _ = writeln!(
                            output,
                            "       {}  {}",
                            theme.dim(id),
                            sanitize_terminal_text(&skipped.reason)
                        );
                        let _ = writeln!(
                            output,
                            "       {}  {}",
                            " ".repeat(id.len()),
                            theme.link(url)
                        );
                    }
                }
            } else {
                let _ = writeln!(
                    output,
                    "       {}",
                    theme.dim("Full skipped-pin details are in JSON and the visual report.")
                );
            }
        }

        if !self.warnings.is_empty() {
            let _ = writeln!(output, "\n{}", theme.warning("WARNINGS"));
            for warning in &self.warnings {
                let _ = writeln!(
                    output,
                    "  {} {}",
                    theme.warning("!"),
                    sanitize_terminal_text(warning)
                );
            }
        }

        if let Some(path) = &self.visual_report {
            let _ = writeln!(
                output,
                "\n{}  {}",
                theme.label("VISUAL REPORT"),
                theme.link(path)
            );
        }

        output
    }

    pub fn render_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

fn match_review_key(kind: &str, scope: MatchScope, items: &[ReportItem]) -> String {
    let mut pin_ids = items
        .iter()
        .map(|item| item.pin_id.as_str())
        .collect::<Vec<_>>();
    pin_ids.sort_unstable();
    format!("{kind}:{}:{}", scope.css_class(), pin_ids.join(","))
}

fn render_items(output: &mut String, items: &[ReportItem], theme: &TextTheme, show_boards: bool) {
    for item in items {
        let status = format!("{:<8}", item.recommendation.label());
        let status = match item.recommendation {
            Recommendation::Keep => theme.success(status),
            Recommendation::Tie => theme.warning(status),
            Recommendation::DeleteCandidate => theme.danger(status),
        };
        let board = match (show_boards, &item.board) {
            (true, Some(board)) => format!("  {}", theme.dim(format!("[{board}]"))),
            _ => String::new(),
        };
        let _ = writeln!(
            output,
            "  {}  {:>5} × {:<5}  {:>10}{}",
            status,
            item.width,
            item.height,
            human_bytes(item.byte_size),
            board
        );
        let _ = writeln!(output, "            {}", theme.link(&item.pin_url));
    }
}

struct TextTheme {
    color: bool,
}

impl TextTheme {
    fn paint(&self, style: Style, value: impl ToString) -> String {
        let value = sanitize_terminal_text(&value.to_string());
        if self.color {
            style.force_styling(true).apply_to(value).to_string()
        } else {
            value
        }
    }

    fn heading(&self, value: impl ToString) -> String {
        self.paint(Style::new().cyan().bold(), value)
    }

    fn section(&self, value: impl ToString) -> String {
        self.paint(Style::new().magenta().bold(), value)
    }

    fn label(&self, value: impl ToString) -> String {
        self.paint(Style::new().blue().bold(), value)
    }

    fn strong(&self, value: impl ToString) -> String {
        self.paint(Style::new().bold(), value)
    }

    fn success(&self, value: impl ToString) -> String {
        self.paint(Style::new().green().bold(), value)
    }

    fn warning(&self, value: impl ToString) -> String {
        self.paint(Style::new().yellow().bold(), value)
    }

    fn danger(&self, value: impl ToString) -> String {
        self.paint(Style::new().red().bold(), value)
    }

    fn accent(&self, value: impl ToString) -> String {
        self.paint(Style::new().magenta().bold(), value)
    }

    fn dim(&self, value: impl ToString) -> String {
        self.paint(Style::new().dim(), value)
    }

    fn link(&self, value: impl ToString) -> String {
        self.paint(Style::new().cyan().underlined(), value)
    }
}

pub(crate) fn human_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    if bytes as f64 >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB)
    } else if bytes as f64 >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB)
    } else {
        format!("{bytes} B")
    }
}

pub(crate) fn rank_tuple(width: u32, height: u32, bytes: u64) -> (u64, u32, u64) {
    (
        u64::from(width) * u64::from(height),
        width.max(height),
        bytes,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scanned(name: &str, pins_found: usize) -> ScannedBoard {
        ScannedBoard {
            name: name.into(),
            url: format!("https://www.pinterest.com/alice/{name}/"),
            pins_reported: Some(pins_found),
            pins_found,
        }
    }

    fn item(pin_id: &str, board: &str, width: u32) -> ReportItem {
        ReportItem {
            pin_id: pin_id.into(),
            pin_url: format!("https://www.pinterest.com/pin/{pin_id}/"),
            board: Some(board.into()),
            image_url: format!("https://i.pinimg.com/originals/{pin_id}.jpg"),
            width,
            height: width,
            byte_size: u64::from(width) * 10,
            recommendation: Recommendation::Keep,
        }
    }

    #[test]
    fn review_keys_ignore_item_order_but_distinguish_match_kind_and_scope() {
        let first = item("1", "Interiors", 1200);
        let second = item("2", "Interiors", 800);
        let exact = DuplicateGroup {
            scope: MatchScope::SameBoard,
            items: vec![first.clone(), second.clone()],
        };
        let reversed = DuplicateGroup {
            scope: MatchScope::SameBoard,
            items: vec![second.clone(), first.clone()],
        };
        let visual = VisualCandidate {
            hash_distance: 1,
            similarity_percent: 99,
            scope: MatchScope::SameBoard,
            items: [first, second],
        };
        let cross_board = DuplicateGroup {
            scope: MatchScope::CrossBoard,
            items: reversed.items.clone(),
        };

        assert_eq!(exact.review_key(), reversed.review_key());
        assert_ne!(exact.review_key(), visual.review_key());
        assert_ne!(exact.review_key(), cross_board.review_key());
    }

    #[test]
    fn review_storage_key_ignores_scan_source_order() {
        let mut report = Report {
            summary: Summary {
                username: Some("alice".into()),
                boards: vec![scanned("Interiors", 1), scanned("Mood board", 1)],
                pins_reported: Some(2),
                pins_found: 2,
                analyzed: 2,
                skipped: 0,
                exact_groups: 0,
                visual_candidates: 0,
            },
            exact_groups: vec![],
            visual_candidates: vec![],
            skipped: vec![],
            warnings: vec![],
            visual_report: None,
        };
        let original = report.review_storage_key();

        report.summary.boards.reverse();
        assert_eq!(original, report.review_storage_key());

        report.summary.username = Some("bob".into());
        assert_ne!(original, report.review_storage_key());
    }

    #[test]
    fn text_report_contains_links_and_recommendations() {
        let report = Report {
            summary: Summary {
                username: None,
                boards: vec![scanned("Ideas", 2)],
                pins_reported: Some(2),
                pins_found: 2,
                analyzed: 2,
                skipped: 0,
                exact_groups: 1,
                visual_candidates: 0,
            },
            exact_groups: vec![DuplicateGroup {
                scope: MatchScope::SameBoard,
                items: vec![ReportItem {
                    pin_id: "123".into(),
                    pin_url: "https://www.pinterest.com/pin/123/".into(),
                    board: Some("Ideas".into()),
                    image_url: "https://i.pinimg.com/originals/example.jpg".into(),
                    width: 1200,
                    height: 800,
                    byte_size: 2048,
                    recommendation: Recommendation::Keep,
                }],
            }],
            visual_candidates: vec![],
            skipped: vec![],
            warnings: vec![],
            visual_report: Some("/tmp/unpin-example.html".into()),
        };

        let rendered = report.render_text();
        assert!(rendered.contains("EXACT 01"));
        assert!(rendered.contains("KEEP"));
        assert!(rendered.contains("https://www.pinterest.com/pin/123/"));
        assert!(rendered.contains("VISUAL REPORT"));
        assert!(rendered.contains("/tmp/unpin-example.html"));
        assert!(rendered.contains("2 returned / 2 reported"));
        assert!(!rendered.contains("\u{1b}["));

        let colored = report.render_text_with_color(true);
        assert!(colored.contains("\u{1b}["));
    }

    #[test]
    fn text_output_sanitizes_dynamic_values_without_changing_json_values() {
        let unsafe_text = "Board\n\u{1b}[31m\t";
        let report = Report {
            summary: Summary {
                username: Some("alice".into()),
                boards: vec![scanned(unsafe_text, 1), scanned("Other", 1)],
                pins_reported: Some(2),
                pins_found: 2,
                analyzed: 2,
                skipped: 1,
                exact_groups: 0,
                visual_candidates: 0,
            },
            exact_groups: vec![],
            visual_candidates: vec![],
            skipped: vec![SkippedPin {
                pin_id: Some("pin\n1".into()),
                pin_url: Some("https://www.pinterest.com/pin/pin-1/".into()),
                reason: "reason\nwith control".into(),
                board: Some(unsafe_text.into()),
            }],
            warnings: vec![format!("warning: {unsafe_text}")],
            visual_report: None,
        };

        let text = report.render_text();
        assert!(
            text.chars()
                .filter(|&character| character != '\n')
                .all(|character| !character.is_control())
        );
        assert!(text.contains("Board�"));
        assert!(text.contains("reason�with control"));
        assert!(text.contains("warning: Board�"));

        let json: serde_json::Value = serde_json::from_str(&report.render_json().unwrap()).unwrap();
        assert_eq!(json["summary"]["boards"][0]["name"], unsafe_text);
        assert_eq!(json["skipped"][0]["reason"], "reason\nwith control");
        assert_eq!(json["warnings"][0], format!("warning: {unsafe_text}"));
    }

    #[test]
    fn json_report_has_required_top_level_keys() {
        let report = Report {
            summary: Summary {
                username: None,
                boards: vec![scanned("Empty", 0)],
                pins_reported: None,
                pins_found: 0,
                analyzed: 0,
                skipped: 0,
                exact_groups: 0,
                visual_candidates: 0,
            },
            exact_groups: vec![],
            visual_candidates: vec![],
            skipped: vec![],
            warnings: vec![],
            visual_report: None,
        };
        let value: serde_json::Value =
            serde_json::from_str(&report.render_json().unwrap()).unwrap();
        for key in [
            "summary",
            "exact_groups",
            "visual_candidates",
            "skipped",
            "warnings",
        ] {
            assert!(value.get(key).is_some(), "{key}");
        }
    }

    #[test]
    fn scope_follows_the_boards_the_items_came_from() {
        let interiors = item("1", "Interiors", 1200);
        let mood = item("2", "Mood board", 600);

        assert_eq!(
            MatchScope::of(&[interiors.clone(), interiors.clone()]),
            MatchScope::SameBoard
        );
        assert_eq!(
            MatchScope::of(&[interiors.clone(), mood]),
            MatchScope::CrossBoard
        );
        assert_eq!(
            MatchScope::of(std::slice::from_ref(&interiors)),
            MatchScope::SameBoard
        );
        assert_eq!(MatchScope::of(&[]), MatchScope::SameBoard);

        // A single-board scan leaves every board unset, which is not "cross".
        let mut unlabeled = interiors;
        unlabeled.board = None;
        assert_eq!(
            MatchScope::of(&[unlabeled.clone(), unlabeled]),
            MatchScope::SameBoard
        );
    }

    #[test]
    fn scope_tags_appear_only_for_multi_board_scans() {
        let mut report = Report {
            summary: Summary {
                username: Some("alice".into()),
                boards: vec![scanned("Interiors", 3), scanned("Mood board", 1)],
                pins_reported: Some(4),
                pins_found: 4,
                analyzed: 4,
                skipped: 0,
                exact_groups: 2,
                visual_candidates: 0,
            },
            exact_groups: vec![
                DuplicateGroup {
                    scope: MatchScope::CrossBoard,
                    items: vec![item("1", "Interiors", 1200), item("2", "Mood board", 600)],
                },
                DuplicateGroup {
                    scope: MatchScope::SameBoard,
                    items: vec![item("3", "Interiors", 800), item("4", "Interiors", 800)],
                },
            ],
            visual_candidates: vec![],
            skipped: vec![],
            warnings: vec![],
            visual_report: None,
        };

        let multi = report.render_text();
        assert!(multi.contains("byte-identical files · ACROSS BOARDS"));
        assert!(multi.contains("byte-identical files · SAME BOARD"));
        // The summary counts only the cross-board matches.
        assert!(multi.contains("1 across boards"));

        // With one board the distinction cannot arise, so it is not mentioned.
        report.summary.boards.truncate(1);
        let single = report.render_text();
        assert!(!single.contains("ACROSS BOARDS"));
        assert!(!single.contains("SAME BOARD"));
        assert!(!single.contains("across boards"));

        // Color reinforces the tag but is never the only carrier of meaning.
        report.summary.boards.push(scanned("Mood board", 1));
        assert!(
            report
                .render_text_with_color(true)
                .contains("ACROSS BOARDS")
        );
    }

    #[test]
    fn scope_counts_span_groups_and_candidates() {
        let mut report = Report {
            summary: Summary {
                username: Some("alice".into()),
                boards: vec![scanned("Interiors", 2), scanned("Mood board", 2)],
                pins_reported: Some(4),
                pins_found: 4,
                analyzed: 4,
                skipped: 0,
                exact_groups: 2,
                visual_candidates: 1,
            },
            exact_groups: vec![
                DuplicateGroup {
                    scope: MatchScope::CrossBoard,
                    items: vec![item("1", "Interiors", 1200)],
                },
                DuplicateGroup {
                    scope: MatchScope::SameBoard,
                    items: vec![item("2", "Interiors", 800)],
                },
            ],
            visual_candidates: vec![VisualCandidate {
                hash_distance: 1,
                similarity_percent: 98,
                scope: MatchScope::SameBoard,
                items: [item("3", "Interiors", 700), item("4", "Interiors", 600)],
            }],
            skipped: vec![],
            warnings: vec![],
            visual_report: None,
        };

        // Two same-board (one group, one candidate) and one cross-board group.
        assert_eq!(report.scope_counts(), (2, 1));
        // The summary line and the HTML tabs must never disagree.
        assert_eq!(report.cross_board_matches(), report.scope_counts().1);

        report.exact_groups.clear();
        report.visual_candidates.clear();
        assert_eq!(report.scope_counts(), (0, 0));
    }

    #[test]
    fn json_reports_the_match_scope() {
        let report = Report {
            summary: Summary {
                username: Some("alice".into()),
                boards: vec![scanned("Interiors", 1), scanned("Mood board", 1)],
                pins_reported: Some(2),
                pins_found: 2,
                analyzed: 2,
                skipped: 0,
                exact_groups: 1,
                visual_candidates: 0,
            },
            exact_groups: vec![DuplicateGroup {
                scope: MatchScope::CrossBoard,
                items: vec![item("1", "Interiors", 1200), item("2", "Mood board", 600)],
            }],
            visual_candidates: vec![],
            skipped: vec![],
            warnings: vec![],
            visual_report: None,
        };

        let value: serde_json::Value =
            serde_json::from_str(&report.render_json().unwrap()).unwrap();
        assert_eq!(value["exact_groups"][0]["scope"], "cross_board");
    }

    #[test]
    fn board_labels_appear_only_for_multi_board_scans() {
        let mut report = Report {
            summary: Summary {
                username: Some("alice".into()),
                boards: vec![scanned("Interiors", 1), scanned("Mood board", 1)],
                pins_reported: Some(2),
                pins_found: 2,
                analyzed: 2,
                skipped: 0,
                exact_groups: 1,
                visual_candidates: 0,
            },
            exact_groups: vec![DuplicateGroup {
                scope: MatchScope::SameBoard,
                items: vec![item("1", "Interiors", 1200), item("2", "Mood board", 600)],
            }],
            visual_candidates: vec![],
            skipped: vec![],
            warnings: vec![],
            visual_report: None,
        };

        let multi = report.render_text();
        assert_eq!(report.title(), "alice 2 boards");
        assert!(multi.contains("alice 2 boards"));
        assert!(multi.contains("[Interiors]"));
        assert!(multi.contains("[Mood board]"));
        assert!(multi.contains("BOARDS"));

        // The same items scanned from one board carry no label at all.
        report.summary.boards.truncate(1);
        report.summary.username = None;
        let single = report.render_text();
        assert_eq!(report.title(), "Interiors");
        assert!(!single.contains("[Interiors]"));
        assert!(!single.contains("BOARDS"));
    }

    #[test]
    fn text_report_groups_large_skipped_sets() {
        let skipped = (0..13)
            .map(|index| SkippedPin {
                pin_id: Some(index.to_string()),
                pin_url: Some(format!("https://www.pinterest.com/pin/{index}/")),
                reason: "video pin".into(),
                board: Some("Videos".into()),
            })
            .collect();
        let report = Report {
            summary: Summary {
                username: None,
                boards: vec![scanned("Videos", 13)],
                pins_reported: Some(13),
                pins_found: 13,
                analyzed: 0,
                skipped: 13,
                exact_groups: 0,
                visual_candidates: 0,
            },
            exact_groups: vec![],
            visual_candidates: vec![],
            skipped,
            warnings: vec![],
            visual_report: None,
        };

        let rendered = report.render_text();
        assert!(rendered.contains("13×"));
        assert!(rendered.contains("video pin"));
        assert!(rendered.contains("Full skipped-pin details"));
        assert!(!rendered.contains("https://www.pinterest.com/pin/0/"));
    }
}
