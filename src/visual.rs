use std::io::{self, Write as IoWrite};
use std::path::{Path, PathBuf};

use askama::Template;
use tempfile::Builder;
use thiserror::Error;

use crate::report::{Report, human_bytes};

#[derive(Template)]
#[template(path = "report.html")]
struct ReportTemplate<'a> {
    report: &'a Report,
    show_boards: bool,
    show_filters: bool,
    same: usize,
    cross: usize,
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
    open_report_with(path, |report_path| open::that(report_path))
}

fn open_report_with(path: &Path, opener: impl FnOnce(&Path) -> io::Result<()>) -> io::Result<()> {
    opener(path)
}

pub fn render_html(report: &Report) -> String {
    let show_boards = report.shows_board_labels();
    let (same, cross) = report.scope_counts();
    ReportTemplate {
        report,
        show_boards,
        show_filters: show_boards && same + cross > 0,
        same,
        cross,
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
        Report {
            summary: Summary {
                username: None,
                boards: vec![ScannedBoard {
                    name: "Ideas <script>alert('x')</script>".into(),
                    url: "https://www.pinterest.com/alice/ideas/".into(),
                    pins_reported: Some(3),
                    pins_found: 3,
                }],
                pins_reported: Some(3),
                pins_found: 3,
                analyzed: 2,
                skipped: 1,
                exact_groups: 1,
                visual_candidates: 1,
            },
            exact_groups: vec![DuplicateGroup {
                scope: MatchScope::SameBoard,
                items: vec![item("101", Recommendation::Keep)],
            }],
            visual_candidates: vec![VisualCandidate {
                hash_distance: 2,
                similarity_percent: 96,
                scope: MatchScope::SameBoard,
                items: [
                    item("101", Recommendation::Keep),
                    item("102", Recommendation::DeleteCandidate),
                ],
            }],
            skipped: vec![SkippedPin {
                pin_id: Some("103".into()),
                pin_url: Some("https://www.pinterest.com/pin/103/".into()),
                reason: "video <unsupported>".into(),
                board: Some("Ideas".into()),
            }],
            warnings: vec!["schema changed & recovered".into()],
            visual_report: None,
        }
    }

    fn item(id: &str, recommendation: Recommendation) -> ReportItem {
        ReportItem {
            pin_id: id.into(),
            pin_url: format!("https://www.pinterest.com/pin/{id}/"),
            board: Some("Ideas".into()),
            image_url: format!("https://i.pinimg.com/originals/{id}.jpg?x=1&y=2"),
            width: 1200,
            height: 800,
            byte_size: 2048,
            recommendation,
        }
    }

    #[test]
    fn html_contains_comparisons_and_escapes_dynamic_values() {
        let html = render_html(&sample_report());

        assert!(html.contains("Exact group 1"));
        assert!(html.contains("Visual candidate 1"));
        assert!(html.contains("96% similar"));
        assert!(html.contains("DELETE?"));
        assert!(html.contains("https://www.pinterest.com/pin/102/"));
        assert!(html.contains("101.jpg?x=1&#38;y=2"));
        assert!(html.contains("Ideas &#60;script&#62;"));
        assert!(html.contains("<span>Pinterest total</span>"));
        assert!(!html.contains("<script>alert"));
        assert!(html.contains("video &#60;unsupported&#62;"));
        assert!(html.contains("overflow: hidden"));
        // Images are height-bounded via max-* only. An explicit width alongside
        // max-height would make both axes definite and stretch the image, and
        // object-fit would paper over that by letterboxing instead.
        assert!(html.contains("max-height: min(60vh, 620px)"));
        assert!(!html.contains("width: 100%; height: auto"));
        assert!(!html.contains("object-fit"));
        assert!(html.contains("class=\"app-shell\""));
        assert!(html.contains("id=\"overview-toggle\""));
        assert!(html.contains("data-review-button"));
        assert!(html.contains("document.addEventListener(\"keydown\""));
        assert!(html.contains("event.target instanceof HTMLButtonElement"));
        assert!(html.contains("prefers-reduced-motion: reduce"));
    }

    #[test]
    fn scope_badges_appear_only_for_multi_board_scans() {
        let mut report = sample_report();
        // The single-board sample must not claim a scope at all.
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
        // The badge sits beside the heading inside the wrapper that keeps
        // .match-heading a two-child flexbox.
        assert!(multi.contains("class=\"match-title\""));
        assert!(multi.contains("<h1>Exact group 1</h1>"));
    }

    /// The single-board `sample_report` plus a second board and a cross-board
    /// group, which is the state the tabs exist for.
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
    fn filter_tabs_appear_only_for_multi_board_scans() {
        let single = render_html(&sample_report());
        assert!(!single.contains("class=\"filters\""));
        assert!(!single.contains("id=\"filter-all\""));

        let multi = render_html(&multi_board_report());
        assert!(multi.contains("role=\"group\""));
        // One exact group (cross) and one visual candidate (same).
        assert!(multi.contains("for=\"filter-all\">All 2</label>"));
        assert!(multi.contains("for=\"filter-same\">Same 1</label>"));
        assert!(multi.contains("for=\"filter-cross\">Cross 1</label>"));
        assert!(multi.contains("id=\"filter-all\""));
        assert!(multi.contains("class=\"filter-input\""));
        assert!(multi.contains("checked"));
    }

    #[test]
    fn match_sections_carry_their_scope_class() {
        let multi = render_html(&multi_board_report());

        assert!(multi.contains("class=\"match cross-board\""));
        assert!(multi.contains("class=\"match same-board\""));
        assert!(multi.contains("data-scope=\"cross-board\""));
        assert!(multi.contains("data-scope=\"same-board\""));
    }

    #[test]
    fn filters_and_match_sections_have_stable_dom_ids() {
        let multi = render_html(&multi_board_report());
        let first_input = multi.find("id=\"filter-all\"").unwrap();
        let first_match = multi.find("id=\"exact-1\"").unwrap();

        // Navigation is emitted before content, and both sides expose stable
        // identifiers for the client-side controller.
        assert!(
            first_input < first_match,
            "filter controls must precede the controlled workspace"
        );
        assert!(multi.contains("data-target=\"exact-1\""));
    }

    #[test]
    fn an_empty_scope_explains_itself() {
        // A single live region handles any empty filtered scope.
        let both = render_html(&multi_board_report());
        assert!(both.contains("id=\"filter-empty\" role=\"status\""));

        // Make every match cross-board; the same-board tab then has nothing.
        let mut report = multi_board_report();
        report.visual_candidates[0].scope = MatchScope::CrossBoard;
        let cross_only = render_html(&report);

        assert!(cross_only.contains("for=\"filter-same\">Same 0</label>"));
        assert!(cross_only.contains("for=\"filter-cross\">Cross 2</label>"));
        assert!(cross_only.contains("No matches in this board scope."));
        assert!(cross_only.contains("visible.length === 0"));
    }

    #[test]
    fn filtering_never_hides_the_details_or_footer() {
        let multi = render_html(&multi_board_report());

        assert!(multi.contains("group.classList.toggle("));
        assert!(multi.contains("\"is-filtered\""));
        assert!(multi.contains("scope !== \"all\" && group.dataset.scope !== scope"));
        assert!(!multi.contains("details.is-filtered"));
        assert!(!multi.contains("footer.is-filtered"));
        // Print explicitly restores matches hidden by focus mode or filtering.
        assert!(multi.contains(".match.is-filtered"));
        assert!(multi.contains("display: block !important"));
    }

    #[test]
    fn review_workspace_has_session_only_navigation_hooks() {
        let html = render_html(&multi_board_report());

        assert!(html.contains("data-reviewed=\"false\""));
        assert!(html.contains("id=\"previous-match\""));
        assert!(html.contains("id=\"next-match\""));
        assert!(html.contains("if (key === \"j\") move(1)"));
        assert!(html.contains("if (key === \"k\") move(-1)"));
        assert!(html.contains("if (key === \"e\" && active)"));
        assert!(html.contains("if (key === \"o\")"));
        assert!(html.contains("setReviewed(active"));
        assert!(html.contains("setActive(target, { announce: true })"));
        assert!(!html.contains("setActive(target, { scroll: true"));
        assert!(!html.contains("localStorage"));
        assert!(!html.contains("sessionStorage"));
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

    #[test]
    fn browser_open_failures_are_returned_to_the_caller() {
        let error = open_report_with(Path::new("/tmp/report.html"), |_| {
            Err(io::Error::new(io::ErrorKind::NotFound, "no browser"))
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }
}
