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

        assert!(multi.contains("<span class=\"badge cross-board\">Across boards</span>"));
        assert!(multi.contains("<span class=\"badge same-board\">Same board</span>"));
        // The badge sits beside the heading inside the wrapper that keeps
        // .match-heading a two-child flexbox.
        assert!(multi.contains("<div class=\"match-title\"><h2>Exact group 1</h2><span"));
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
        assert!(!single.contains("scope-filter"));

        let multi = render_html(&multi_board_report());
        assert!(multi.contains("role=\"group\""));
        // One exact group (cross) and one visual candidate (same).
        assert!(multi.contains("<label for=\"filter-all\">All <span>2</span></label>"));
        assert!(multi.contains("<label for=\"filter-same\">Same board <span>1</span></label>"));
        assert!(multi.contains("<label for=\"filter-cross\">Across boards <span>1</span></label>"));
        assert!(multi.contains("id=\"filter-all\" class=\"filter-input\" checked"));
    }

    #[test]
    fn match_sections_carry_their_scope_class() {
        let multi = render_html(&multi_board_report());

        assert!(multi.contains("<section class=\"match cross-board\">"));
        assert!(multi.contains("<section class=\"match same-board\">"));
    }

    #[test]
    fn filter_inputs_precede_every_match_section() {
        let multi = render_html(&multi_board_report());
        let first_input = multi.find("id=\"filter-all\"").unwrap();
        let first_match = multi.find("<section class=\"match").unwrap();

        // The filtering CSS uses `~`, which only reaches later siblings, so this
        // ordering is load-bearing rather than cosmetic.
        assert!(
            first_input < first_match,
            "inputs must come before the matches they filter"
        );
    }

    #[test]
    fn an_empty_scope_explains_itself() {
        // Both scopes present, so neither tab needs an empty state. Match the
        // div, not the bare class name, which also occurs in the stylesheet.
        let both = render_html(&multi_board_report());
        assert!(!both.contains("<div class=\"empty filter-empty"));

        // Make every match cross-board; the same-board tab then has nothing.
        let mut report = multi_board_report();
        report.visual_candidates[0].scope = MatchScope::CrossBoard;
        let cross_only = render_html(&report);

        assert!(cross_only.contains("<div class=\"empty filter-empty same-board\">"));
        assert!(!cross_only.contains("<div class=\"empty filter-empty cross-board"));
        assert!(cross_only.contains("No duplicates within a single board."));
        assert!(
            cross_only.contains("<label for=\"filter-same\">Same board <span>0</span></label>")
        );
    }

    #[test]
    fn filtering_never_hides_the_details_or_footer() {
        let multi = render_html(&multi_board_report());

        // Only `.match` and `.filter-empty` may be hidden by a checked filter.
        for rule in [
            "#filter-same:checked ~ .match.cross-board",
            "#filter-cross:checked ~ .match.same-board",
        ] {
            assert!(multi.contains(rule), "{rule}");
        }
        assert!(!multi.contains(":checked ~ details"));
        assert!(!multi.contains(":checked ~ footer"));
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
