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
        assert!(html.contains("These images are 96%"));
        assert!(html.contains("These pins use the same image file."));
        assert!(html.contains("keep/delete suggestions"));
        assert!(html.contains("never changes your Pinterest account."));
        assert!(html.contains("DELETE?"));
        assert!(html.contains("https://www.pinterest.com/pin/102/"));
        assert!(html.contains("101.jpg?x=1&#38;y=2"));
        assert!(html.contains("Ideas &#60;script&#62;"));
        assert!(!html.contains("<span>Pinterest total</span>"));
        assert!(!html.contains("class=\"stats\""));
        assert!(!html.contains("<script>alert"));
        assert!(html.contains("video &#60;unsupported&#62;"));
        assert!(html.contains("overflow: hidden"));
        // Intrinsic dimensions reserve each lazy image's final aspect ratio
        // before decode, while the viewport cap prevents overly tall cards.
        assert!(html.contains("max-height: min(68dvh, 720px)"));
        assert!(html.contains("width=\"1200\""));
        assert!(html.contains("height=\"800\""));
        assert!(
            html.contains("aria-label=\"Open original image, 1200 × 800 pixels, for pin 101\"")
        );
        assert!(html.contains("alt=\"\""));
        assert!(html.contains("object-fit: contain"));
        assert!(html.contains(
            "body:not(.overview-mode) .image-stage img {\n                position: static;\n                width: auto;\n                height: auto;\n                min-width: 0;\n                min-height: 0;\n                max-width: 100%;\n                max-height: 100%;\n                justify-self: center;\n                align-self: center;\n                object-fit: contain;"
        ));
        assert!(html.contains("class=\"app-shell\""));
        assert!(html.contains("id=\"overview-toggle\""));
        assert!(html.contains("Overview is a scan queue"));
        assert!(
            html.contains(".overview-mode .image-stage { aspect-ratio: 4 / 5; min-height: 0; }")
        );
        assert!(html.contains(
            ".overview-mode .image-stage img {\n                width: auto;\n                height: auto;\n                min-width: 0;\n                min-height: 0;\n                max-width: 100%;\n                max-height: 100%;\n                justify-self: center;\n                align-self: center;\n                object-fit: contain;"
        ));
        assert!(html.contains(".overview-mode .match { position: static; max-width: none;"));
        assert!(html.contains("data-review-button"));
        assert!(html.contains(".progress-fill {\n                display: block;"));
        assert!(html.contains("document.addEventListener(\"keydown\""));
        assert!(html.contains("event.target instanceof HTMLButtonElement"));
        assert!(html.contains("prefers-reduced-motion: reduce"));
    }

    #[test]
    fn html_includes_the_report_design_and_accessibility_baseline() {
        let html = render_html(&sample_report());

        assert!(html.contains("font-family: Geist, \"Helvetica Neue\", Arial, sans-serif"));
        assert!(html.contains("min-height: 100dvh"));
        assert!(!html.contains("radial-gradient"));
        assert!(!html.contains("linear-gradient"));
        assert!(html.contains("class=\"control-icon\""));
        assert!(html.contains("@media (pointer: coarse)"));
        assert!(html.contains(".control.icon-only { width: 44px; min-height: 44px; }"));
        assert!(html.contains("scrollbar-color: #494949 var(--shell)"));
        assert!(html.contains(".match-heading > :first-child { min-width: 0; }"));
        assert!(html.contains(".match-heading { display: grid; gap: 20px; }"));
        assert!(!html.contains("font-family:\n                    Inter"));
        assert!(html.contains("class=\"skip-link\" href=\"#report-content\""));
        assert!(html.contains("id=\"report-content\" tabindex=\"-1\""));
        assert!(html.contains("name=\"description\""));
        assert!(html.contains("name=\"robots\" content=\"noindex, nofollow\""));
        assert!(html.contains("rel=\"icon\""));
    }

    #[test]
    fn html_does_not_load_remote_scripts() {
        let html = render_html(&sample_report());

        assert!(!html.contains("<script src="));
        assert!(!html.contains("cdn.jsdelivr.net"));
        assert!(!html.contains("gsap"));
        assert!(!html.contains("ScrollTrigger"));
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
        assert!(!single.contains("id=\"filter-all\""));
        assert!(!single.contains("aria-label=\"Filter matches by board scope\""));

        let multi = render_html(&multi_board_report());
        assert!(multi.contains("role=\"group\""));
        // One exact group (cross) and one visual candidate (same).
        assert!(multi.contains("Board scope</span>"));
        assert!(multi.contains(
            "for=\"filter-all\"><span>All</span><span class=\"filter-count\">2</span></label>"
        ));
        assert!(multi.contains(
            "for=\"filter-same\"><span>Same</span><span class=\"filter-count\">1</span></label>"
        ));
        assert!(multi.contains(
            "for=\"filter-cross\"><span>Cross</span><span class=\"filter-count\">1</span></label>"
        ));
        assert!(multi.contains("id=\"filter-all\""));
        assert!(multi.contains("class=\"filter-input\""));
        assert!(multi.contains("checked"));
    }

    #[test]
    fn kind_filters_appear_only_for_mixed_match_types() {
        let mixed = render_html(&sample_report());
        assert!(mixed.contains("aria-label=\"Filter matches by type\""));
        assert!(mixed.contains("id=\"kind-all\""));
        assert!(mixed.contains("Match type</span>"));
        assert!(mixed.contains(
            "for=\"kind-all\"><span>All</span><span class=\"filter-count\">2</span></label>"
        ));
        assert!(mixed.contains(
            "for=\"kind-exact\"><span>Exact</span><span class=\"filter-count\">1</span></label>"
        ));
        assert!(mixed.contains(
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
    fn unreviewed_filter_and_visible_count_follow_queue_presence() {
        let populated = render_html(&sample_report());
        assert!(populated.contains("id=\"unreviewed-only\""));
        assert!(populated.contains("Unreviewed only"));
        assert!(populated.contains("id=\"visible-count\""));
        assert!(populated.contains("2 / 2 shown"));

        let mut empty = sample_report();
        empty.exact_groups.clear();
        empty.visual_candidates.clear();
        let empty = render_html(&empty);
        assert!(!empty.contains("id=\"unreviewed-only\""));
        assert!(!empty.contains("id=\"visible-count\""));
    }

    #[test]
    fn match_navigation_and_sections_carry_kind_metadata() {
        let html = render_html(&sample_report());

        assert!(html.contains("data-target=\"exact-1\"\n                            data-scope=\"same-board\"\n                            data-kind=\"exact\""));
        assert!(html.contains("id=\"exact-1\"\n                        data-group\n                        data-scope=\"same-board\"\n                        data-kind=\"exact\""));
        assert!(html.contains("data-target=\"visual-1\"\n                            data-scope=\"same-board\"\n                            data-kind=\"visual\""));
        assert!(html.contains("id=\"visual-1\"\n                        data-group\n                        data-scope=\"same-board\"\n                        data-kind=\"visual\""));
    }

    #[test]
    fn filter_controller_composes_scope_kind_and_review_state() {
        let html = render_html(&multi_board_report());

        assert!(html.contains("const scopeMatches ="));
        assert!(html.contains("const kindMatches ="));
        assert!(html.contains("const reviewMatches ="));
        assert!(html.contains("!(scopeMatches && kindMatches && reviewMatches)"));
        assert!(html.contains("const link = links.find("));
        assert!(
            html.contains("applyFilter(preferredActive);\n                    updateProgress();")
        );
        assert!(
            html.contains("preferredActive = visible[index + 1] || visible[index - 1] || null")
        );
        assert!(
            html.contains(
                "visibleCount.textContent = `${visible.length} / ${groups.length} shown`"
            )
        );
        assert!(html.contains("No matches in the current filters."));
    }

    /// Focus view renders whichever match carries `is-active`, so exactly one
    /// element may hold it. Filtering the active match out used to reassign the
    /// tracking variable before clearing the class, stranding it on the hidden
    /// match; returning to "All" then drew that stale match below the real one.
    #[test]
    fn filtering_away_the_active_match_clears_it_before_reassigning() {
        let multi = render_html(&multi_board_report());

        assert!(multi.contains(".js body:not(.overview-mode) .match:not(.is-active)"));
        assert!(
            !multi.contains("if (!visible.includes(active)) active = visible[0]"),
            "reassigning first hands setActive the replacement, so the outgoing \
             match keeps is-active and reappears alongside the active one"
        );
        assert!(multi.contains("active?.classList.remove(\"is-active\");\n                        active = visible[0] || null;"));
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

        assert!(cross_only.contains(
            "for=\"filter-same\"><span>Same</span><span class=\"filter-count\">0</span></label>"
        ));
        assert!(cross_only.contains(
            "for=\"filter-cross\"><span>Cross</span><span class=\"filter-count\">2</span></label>"
        ));
        assert!(cross_only.contains("No matches in the current filters."));
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
        assert!(html.contains("class=\"match-link-state\" aria-hidden=\"true\""));
        assert!(html.contains(">Reviewed</span"));
        assert!(!html.contains("class=\"scope-dot\""));
        assert!(html.contains("id=\"previous-match\""));
        assert!(html.contains("id=\"next-match\""));
        assert!(html.contains("if (key === \"j\") move(1)"));
        assert!(html.contains("if (key === \"k\") move(-1)"));
        assert!(html.contains("if (key === \"e\" && active)"));
        assert!(html.contains("if (key === \"o\")"));
        assert!(html.contains("setReviewed(reviewedGroup, reviewed)"));
        assert!(html.contains("${reviewStatus}. ${visibleGroups().length} matches shown"));
        assert!(html.contains("selected. ${visibleGroups().length} matches shown"));
        assert!(html.contains("`${title}, reviewed`"));
        assert!(!html.contains("class=\"match-link-meta\""));
        assert!(html.contains("const acknowledgeReview"));
        assert!(html.contains("progressBlock.classList.add(\"is-updated\")"));
        assert!(html.contains("@keyframes review-confirmation"));
        assert!(html.contains("setActive(target, { announce: true })"));
        assert!(!html.contains("scroll: true"));
        assert!(!html.contains("scrollIntoView"));
        assert!(html.contains("advanceAfterReview(group)"));
        assert!(html.contains("advanceAfterReview(reviewedGroup)"));
        assert!(html.contains("const warmGroupImages = (group)"));
        assert!(html.contains("const initializeImages = () =>"));
        assert!(html.contains("initializeImages();"));
        assert!(html.contains("warmNearbyImages();"));
        assert!(html.contains("image.loading = \"eager\""));
        assert!(html.contains("link.addEventListener(\"pointerenter\", warmTarget)"));
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
