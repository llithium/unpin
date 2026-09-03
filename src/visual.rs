use std::io::{self, Write as IoWrite};
use std::path::{Path, PathBuf};

use askama::Template;
use tempfile::Builder;
use thiserror::Error;

use crate::report::{MatchScope, Report, human_bytes};

#[derive(Template)]
#[template(path = "report.html")]
struct ReportTemplate<'a> {
    report: &'a Report,
    show_boards: bool,
    same: usize,
    cross: usize,
    quick_wins: usize,
}

impl ReportTemplate<'_> {
    fn human_bytes(&self, bytes: &u64) -> String {
        human_bytes(*bytes)
    }
}

#[derive(Debug, Error)]
pub enum VisualError {
    #[error("failed to create the temporary HTML report")]
    Create(#[source] io::Error),

    #[error("failed to write the temporary HTML report")]
    Write(#[source] io::Error),

    #[error("failed to retain the temporary HTML report")]
    Persist(#[source] io::Error),
}

pub fn create_temporary_report(report: &Report) -> Result<PathBuf, VisualError> {
    let mut file = Builder::new()
        .prefix("unpin-")
        .suffix(".html")
        .tempfile()
        .map_err(VisualError::Create)?;
    file.write_all(render_html(report).as_bytes())
        .and_then(|_| file.flush())
        .and_then(|_| file.as_file().sync_all())
        .map_err(VisualError::Write)?;

    file.keep()
        .map(|(_, path)| path)
        .map_err(|error| VisualError::Persist(error.error))
}

pub fn open_report(path: &Path) -> io::Result<()> {
    open::that(path)
}

pub fn render_html(report: &Report) -> String {
    let show_boards = report.shows_board_labels();
    let (same, cross) = report.scope_counts();
    let quick_wins = report
        .exact_groups
        .iter()
        .filter(|group| group.scope == MatchScope::SameBoard)
        .count();
    ReportTemplate {
        report,
        show_boards,
        same,
        cross,
        quick_wins,
    }
    .render()
    .expect("the HTML report template is valid")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::pinterest::SkippedPin;
    use crate::report::{
        DuplicateGroup, MatchScope, Recommendation, ReportItem, ScannedBoard, Summary,
        VisualCandidate,
    };

    fn sample_report() -> Report {
        let mut exact_item = item("102", Recommendation::Tie);
        exact_item.image_url = "https://i.pinimg.com/originals/101.jpg?x=1&y=2".into();
        let mut visual_item = item("104", Recommendation::DeleteCandidate);
        visual_item.width = 1190;
        visual_item.byte_size = 2000;
        Report {
            summary: Summary {
                username: None,
                boards: vec![ScannedBoard {
                    name: "Ideas <script>alert('x')</script>".into(),
                    url: "https://www.pinterest.com/alice/ideas/".into(),
                    pins_reported: Some(5),
                    pins_found: 5,
                }],
                pins_reported: Some(5),
                pins_found: 5,
                analyzed: 4,
                skipped: 1,
                exact_groups: 1,
                visual_candidates: 1,
            },
            exact_groups: vec![DuplicateGroup {
                scope: MatchScope::SameBoard,
                items: vec![item("101", Recommendation::Tie), exact_item],
            }],
            visual_candidates: vec![VisualCandidate {
                hash_distance: 2,
                similarity_percent: 98,
                scope: MatchScope::SameBoard,
                items: [item("103", Recommendation::Keep), visual_item],
            }],
            skipped: vec![SkippedPin {
                pin_id: Some("105".into()),
                pin_url: Some("https://www.pinterest.com/pin/105/".into()),
                reason: "video <unsupported>".into(),
                board: Some("Ideas".into()),
            }],
            warnings: vec!["schema changed & recovered".into()],
        }
    }

    fn item(id: &str, recommendation: Recommendation) -> ReportItem {
        ReportItem {
            pin_id: id.into(),
            pin_url: format!("https://www.pinterest.com/pin/{id}/"),
            board: Some("Ideas".into()),
            source_id: None,
            image_url: format!("https://i.pinimg.com/originals/{id}.jpg?x=1&y=2"),
            width: 1200,
            height: 800,
            byte_size: 2048,
            recommendation,
        }
    }

    fn contains_html_fragment(html: &str, fragment: &str) -> bool {
        fn normalize(value: &str) -> String {
            value
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .replace(" >", ">")
                .replace("< ", "<")
        }

        normalize(html).contains(normalize(fragment).as_str())
    }

    #[test]
    fn html_contains_report_data_and_escapes_dynamic_values() {
        let html = render_html(&sample_report());

        assert!(html.contains("Exact group 1"));
        assert!(html.contains("Visual candidate 1"));
        assert!(html.contains("These images are 98%"));
        assert!(html.contains("These pins use the same image file."));
        assert!(html.contains("keep/delete suggestions"));
        assert!(html.contains("DELETE?"));
        assert!(html.contains("https://www.pinterest.com/pin/102/"));
        assert!(html.contains("101.jpg?x=1&#38;y=2"));
        assert!(html.contains("Ideas &#60;script&#62;"));
        assert!(!html.contains("<script>alert"));
        assert!(html.contains("video &#60;unsupported&#62;"));
        assert!(html.contains("width=\"1200\""));
        assert!(html.contains("height=\"800\""));
        assert!(
            html.contains("aria-label=\"Open original image, 1200 × 800 pixels, for pin 101\"")
        );
        assert!(html.contains("class=\"app-shell\""));
        assert!(html.contains("id=\"overview-toggle\""));
        assert!(html.contains("id=\"quick-wins\""));
        assert!(html.contains("id=\"reset-review\""));
        assert!(html.contains(
            "aria-label=\"Show exact duplicates saved more than once within one board\""
        ));
        assert!(html.contains("Review marks are kept"));
        assert!(html.contains("unpin never changes your Pinterest account."));
        assert!(!html.contains("<script src="));
    }

    #[test]
    fn scope_badges_appear_only_for_multi_board_scans() {
        let mut report = sample_report();
        let single = render_html(&report);
        assert!(!single.contains("badge same-board"));
        assert!(!single.contains("badge cross-board"));
        assert!(!single.contains("Across boards"));

        report.summary.boards.push(ScannedBoard {
            name: "Mood board".into(),
            url: "https://www.pinterest.com/alice/mood-board/".into(),
            pins_reported: Some(1),
            pins_found: 1,
        });
        report.exact_groups[0].scope = MatchScope::CrossBoard;
        let multi = render_html(&report);
        assert!(multi.contains("class=\"badge cross-board\""));
        assert!(multi.contains("Across boards</span"));
        assert!(multi.contains("class=\"badge same-board\""));
        assert!(multi.contains("Same board</span"));
    }

    fn multi_board_report() -> Report {
        let mut report = sample_report();
        report.summary.boards.push(ScannedBoard {
            name: "Mood board".into(),
            url: "https://www.pinterest.com/alice/mood-board/".into(),
            pins_reported: Some(1),
            pins_found: 1,
        });
        report.exact_groups[0].scope = MatchScope::CrossBoard;
        report
    }

    #[test]
    fn filter_controls_match_the_report_scopes_and_kinds() {
        let single = render_html(&sample_report());
        assert!(!single.contains("id=\"filter-all\""));
        assert!(!single.contains("aria-label=\"Filter matches by board scope\""));

        let mixed = render_html(&multi_board_report());
        assert!(mixed.contains("aria-label=\"Filter matches by board scope\""));
        assert!(contains_html_fragment(
            &mixed,
            "for=\"filter-all\"><span>All</span><span class=\"filter-count\">2</span></label>"
        ));
        assert!(contains_html_fragment(
            &mixed,
            "for=\"filter-same\"><span>Same</span><span class=\"filter-count\">1</span></label>"
        ));
        assert!(contains_html_fragment(
            &mixed,
            "for=\"filter-cross\"><span>Cross</span><span class=\"filter-count\">1</span></label>"
        ));
        assert!(mixed.contains("aria-label=\"Filter matches by type\""));
        assert!(contains_html_fragment(
            &mixed,
            "for=\"kind-exact\"><span>Exact</span><span class=\"filter-count\">1</span></label>"
        ));
        assert!(contains_html_fragment(
            &mixed,
            "for=\"kind-visual\"><span>Visual</span><span class=\"filter-count\">1</span></label>"
        ));

        let mut exact_only = sample_report();
        exact_only.visual_candidates.clear();
        assert!(!render_html(&exact_only).contains("id=\"kind-all\""));

        let mut visual_only = sample_report();
        visual_only.exact_groups.clear();
        assert!(!render_html(&visual_only).contains("id=\"kind-all\""));
    }

    #[test]
    fn quick_wins_control_is_conditional_and_counts_same_board_exact_groups() {
        let mut exact_only = sample_report();
        exact_only.visual_candidates.clear();
        assert!(!render_html(&exact_only).contains("id=\"quick-wins\""));

        let mut multi_report = sample_report();
        multi_report.summary.boards.push(ScannedBoard {
            name: "Mood board".into(),
            url: "https://www.pinterest.com/alice/mood-board/".into(),
            pins_reported: Some(1),
            pins_found: 1,
        });
        let multi = render_html(&multi_report);
        assert!(multi.contains("id=\"quick-wins\""));
        assert!(contains_html_fragment(
            &multi,
            "<span>Quick wins</span> <span class=\"filter-count\">1</span>"
        ));
    }

    #[test]
    fn empty_reports_omit_match_controls() {
        let mut empty = sample_report();
        empty.exact_groups.clear();
        empty.visual_candidates.clear();
        let html = render_html(&empty);
        assert!(!html.contains("id=\"unreviewed-only\""));
        assert!(!html.contains("id=\"visible-count\""));
        assert!(!html.contains("id=\"quick-wins\""));
    }

    #[test]
    fn match_markup_carries_stable_scope_kind_and_review_identity() {
        let multi = render_html(&multi_board_report());
        assert!(contains_html_fragment(
            &multi,
            "data-target=\"exact-1\" data-scope=\"cross-board\" data-kind=\"exact\""
        ));
        assert!(contains_html_fragment(
            &multi,
            "id=\"exact-1\" data-group data-scope=\"cross-board\" data-kind=\"exact\""
        ));
        assert!(multi.contains("data-target=\"visual-1\""));
        assert!(multi.contains("data-kind=\"visual\""));
        assert!(multi.contains("data-review-key=\"exact:cross-board:101,102\""));
        assert!(multi.contains("data-review-key=\"visual:same-board:103,104\""));
        assert!(multi.contains("id=\"filter-all\""));
        assert!(multi.find("id=\"filter-all\"").unwrap() < multi.find("id=\"exact-1\"").unwrap());
    }

    #[test]
    fn an_empty_scope_explains_itself() {
        let both = render_html(&multi_board_report());
        assert!(both.contains("id=\"filter-empty\" role=\"status\""));

        let mut report = multi_board_report();
        report.visual_candidates[0].scope = MatchScope::CrossBoard;
        let cross_only = render_html(&report);
        assert!(contains_html_fragment(
            &cross_only,
            "for=\"filter-same\"><span>Same</span><span class=\"filter-count\">0</span></label>"
        ));
        assert!(contains_html_fragment(
            &cross_only,
            "for=\"filter-cross\"><span>Cross</span><span class=\"filter-count\">2</span></label>"
        ));
    }

    #[test]
    fn temporary_reports_are_unique_and_retained() {
        let first = create_temporary_report(&sample_report()).unwrap();
        let second = create_temporary_report(&sample_report()).unwrap();

        assert_ne!(first, second);
        assert_eq!(
            first.extension().and_then(|value| value.to_str()),
            Some("html")
        );
        assert!(first.starts_with(std::env::temp_dir()));
        assert!(
            fs::read_to_string(&first)
                .unwrap()
                .contains("<!doctype html>")
        );

        fs::remove_file(first).unwrap();
        fs::remove_file(second).unwrap();
    }
}
