use std::fmt::Write as FmtWrite;
use std::io::{self, Write as IoWrite};
use std::path::{Path, PathBuf};

use tempfile::Builder;
use thiserror::Error;

use crate::report::{MatchScope, Report, ReportItem, human_bytes};

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
    let mut html = String::with_capacity(16 * 1024);
    html.push_str(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>unpin comparison report</title>
<style>
:root {
  color-scheme: light;
  --ink: #111820;
  --muted: #566474;
  --paper: #e9eef2;
  --card: #f9fbfc;
  --line: #aebbc7;
  --signal: #006d77;
  --red: #b42336;
  --red-soft: #fde8eb;
  --green: #08704f;
  --green-soft: #dff5eb;
  --gold: #735500;
  --gold-soft: #fff0b8;
  font-family: "Avenir Next", Avenir, "Segoe UI", sans-serif;
}
* { box-sizing: border-box; }
body { margin: 0; background: var(--paper); color: var(--ink); }
main { width: min(1440px, calc(100% - 32px)); margin: 0 auto; padding: 52px 0 80px; }
header { display: grid; gap: 15px; margin-bottom: 44px; padding-left: 18px; border-left: 5px solid var(--signal); }
.eyebrow { margin: 0; color: var(--signal); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: .73rem; font-weight: 800; letter-spacing: .14em; text-transform: uppercase; }
h1 { max-width: 1100px; margin: 0; font-family: "Avenir Next Condensed", "Arial Narrow", sans-serif; font-size: clamp(2.5rem, 6vw, 5.7rem); font-stretch: condensed; font-weight: 800; line-height: .9; letter-spacing: -.045em; text-transform: uppercase; }
.lede { max-width: 700px; margin: 0; color: var(--muted); font-size: 1.03rem; line-height: 1.6; }
.stats { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 1px; overflow: hidden; margin-left: -18px; border: 1px solid var(--line); border-radius: 3px; background: var(--line); }
.stat { padding: 18px; background: var(--card); }
.stat strong { display: block; font-family: "Avenir Next Condensed", "Arial Narrow", sans-serif; font-size: 2.2rem; font-weight: 800; line-height: 1; }
.stat span { color: var(--muted); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: .7rem; font-weight: 700; letter-spacing: .06em; text-transform: uppercase; }
.match { margin-top: 48px; }
.match-heading { display: flex; align-items: end; justify-content: space-between; gap: 20px; padding-bottom: 12px; border-bottom: 1px solid var(--line); }
.match-title { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; }
.match-heading h2 { margin: 0; font-family: "Avenir Next Condensed", "Arial Narrow", sans-serif; font-size: clamp(1.55rem, 3vw, 2.35rem); font-weight: 800; letter-spacing: -.02em; text-transform: uppercase; }
.match-heading p { margin: 0; color: var(--muted); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: .76rem; text-align: right; text-transform: uppercase; }
.comparison { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 310px), 1fr)); gap: 18px; margin-top: 18px; }
.pin-card { min-width: 0; overflow: hidden; border: 1px solid #8293a2; border-radius: 3px; background: var(--card); box-shadow: 5px 5px 0 rgb(91 110 125 / 14%); }
.image-stage { position: relative; display: block; min-width: 0; overflow: hidden; background: #d9e2e8; line-height: 0; }
.image-stage::before { content: "IMAGE FIELD"; position: absolute; top: 7px; left: 9px; z-index: 1; padding: 2px 5px; color: #344655; background: rgb(249 251 252 / 88%); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: .58rem; font-weight: 800; letter-spacing: .12em; }
.image-stage::after { content: ""; position: absolute; inset: 7px; pointer-events: none; background: linear-gradient(var(--signal), var(--signal)) left top / 16px 2px no-repeat, linear-gradient(var(--signal), var(--signal)) left top / 2px 16px no-repeat, linear-gradient(var(--signal), var(--signal)) right bottom / 16px 2px no-repeat, linear-gradient(var(--signal), var(--signal)) right bottom / 2px 16px no-repeat; }
/* Bounded by both maxes with no explicit width or height, so a tall pin cannot
   fill the viewport and the box keeps the image's own aspect ratio. Setting
   width: 100% alongside max-height would make both axes definite and stretch
   the image. */
.image-stage img { display: block; max-width: 100%; max-height: min(60vh, 620px); margin: 0 auto; }
.card-body { display: grid; gap: 13px; padding: 16px; }
.card-top { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.badge { display: inline-flex; align-items: center; min-height: 27px; padding: 4px 9px; border-radius: 999px; font-size: .72rem; font-weight: 850; letter-spacing: .08em; }
.badge.keep { color: var(--green); background: var(--green-soft); }
.badge.tie { color: var(--gold); background: var(--gold-soft); }
.badge.delete { color: var(--red); background: var(--red-soft); }
.badge.cross-board { color: var(--gold); background: var(--gold-soft); }
.badge.same-board { color: var(--muted); border: 1px solid var(--line); }
.pin-id { overflow: hidden; color: var(--muted); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: .76rem; text-overflow: ellipsis; }
.board { justify-self: start; max-width: 100%; overflow: hidden; padding: 2px 9px; border: 1px solid var(--line); border-radius: 999px; color: var(--muted); font-size: .74rem; text-overflow: ellipsis; white-space: nowrap; }
.boards { margin: 14px 0 0; color: var(--muted); font-size: .9rem; line-height: 1.7; }
.dimensions { margin: 0; font-family: "Avenir Next Condensed", "Arial Narrow", sans-serif; font-size: 1.5rem; font-weight: 750; }
.dimensions span { color: var(--muted); font-family: ui-sans-serif, sans-serif; font-size: .82rem; }
.actions { display: flex; flex-wrap: wrap; gap: 8px; }
.button { display: inline-flex; align-items: center; justify-content: center; min-height: 38px; padding: 8px 12px; border: 1px solid var(--ink); border-radius: 2px; color: var(--ink); font-size: .82rem; font-weight: 750; text-decoration: none; }
.button.primary { color: white; background: var(--ink); }
.button:hover { transform: translateY(-1px); }
.button:focus-visible, .image-stage:focus-visible, summary:focus-visible { outline: 3px solid #00a6b2; outline-offset: 3px; }
.empty { margin-top: 42px; padding: 32px; border: 1px dashed var(--line); border-radius: 3px; color: var(--muted); text-align: center; background: var(--card); }
.filter-input { position: absolute; width: 1px; height: 1px; margin: 0; padding: 0; overflow: hidden; opacity: 0; }
.filters { display: flex; flex-wrap: wrap; gap: 8px; margin-top: 38px; }
.filters label { display: inline-flex; align-items: center; gap: 8px; padding: 9px 15px; border: 1px solid var(--line); border-radius: 2px; background: var(--card); color: var(--muted); cursor: pointer; font-size: .85rem; font-weight: 750; }
.filters label span { padding: 1px 7px; border-radius: 999px; background: var(--paper); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: .74rem; }
.filters label:hover { border-color: var(--signal); color: var(--ink); }
/* Checked and focus styling forward from the hidden input to its own label. */
#filter-all:checked ~ .filters label[for="filter-all"],
#filter-same:checked ~ .filters label[for="filter-same"],
#filter-cross:checked ~ .filters label[for="filter-cross"] { border-color: var(--ink); background: var(--ink); color: white; }
#filter-all:checked ~ .filters label[for="filter-all"] span,
#filter-same:checked ~ .filters label[for="filter-same"] span,
#filter-cross:checked ~ .filters label[for="filter-cross"] span { background: rgb(255 255 255 / 22%); }
#filter-all:focus-visible ~ .filters label[for="filter-all"],
#filter-same:focus-visible ~ .filters label[for="filter-same"],
#filter-cross:focus-visible ~ .filters label[for="filter-cross"] { outline: 3px solid #00a6b2; outline-offset: 3px; }
#filter-same:checked ~ .match.cross-board,
#filter-cross:checked ~ .match.same-board { display: none; }
.filter-empty { display: none; }
#filter-same:checked ~ .filter-empty.same-board,
#filter-cross:checked ~ .filter-empty.cross-board { display: block; }
details { margin-top: 30px; padding: 16px 18px; border: 1px solid var(--line); border-radius: 3px; background: var(--card); }
summary { cursor: pointer; font-weight: 750; }
details ul { margin-bottom: 0; padding-left: 20px; color: var(--muted); line-height: 1.6; }
footer { margin-top: 52px; padding-top: 18px; border-top: 1px solid var(--line); color: var(--muted); font-size: .8rem; }
@media (prefers-reduced-motion: reduce) { .button:hover { transform: none; } }
/* Print every match: a filtered printout would silently drop the hidden ones. */
@media print {
  .filters, .filter-empty { display: none !important; }
  #filter-same:checked ~ .match.cross-board,
  #filter-cross:checked ~ .match.same-board { display: block !important; }
}
@media (max-width: 620px) {
  main { width: min(100% - 20px, 1440px); padding-top: 30px; }
  .match-heading { align-items: start; flex-direction: column; }
  .match-heading p { text-align: left; }
}
</style>
</head>
<body>
<main>
<header>
<p class="eyebrow">Pinterest duplicate review</p>
"#,
    );

    let show_boards = report.shows_board_labels();
    let _ = writeln!(html, "<h1>{}</h1>", escape_html(&report.title()));
    html.push_str(
        "<p class=\"lede\">Compare likely duplicate pins visually. Recommendations favor pixel area, longest edge, and file size. Review every candidate before deleting anything.</p>\n",
    );
    let _ = writeln!(
        html,
        "<div class=\"stats\"><div class=\"stat\"><strong>{}</strong><span>Pins returned</span></div>{}<div class=\"stat\"><strong>{}</strong><span>Analyzed</span></div><div class=\"stat\"><strong>{}</strong><span>Skipped</span></div><div class=\"stat\"><strong>{}</strong><span>Exact groups</span></div><div class=\"stat\"><strong>{}</strong><span>Visual pairs</span></div></div>",
        report.summary.pins_found,
        report
            .summary
            .pins_reported
            .map(|reported| format!(
                "<div class=\"stat\"><strong>{reported}</strong><span>Pinterest total</span></div>"
            ))
            .unwrap_or_default(),
        report.summary.analyzed,
        report.summary.skipped,
        report.summary.exact_groups,
        report.summary.visual_candidates
    );
    if show_boards {
        html.push_str("<p class=\"boards\">Scanned ");
        for (index, board) in report.summary.boards.iter().enumerate() {
            let separator = if index == 0 { "" } else { " · " };
            let name = escape_html(&board.name);
            // Pinterest does not always supply a board URL. An empty href would
            // link back to the report file itself, so render plain text instead.
            let named = if board.url.is_empty() {
                name
            } else {
                format!(
                    "<a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\">{name}</a>",
                    escape_html(&board.url)
                )
            };
            let _ = write!(html, "{separator}{named} ({} pins)", board.pins_found);
        }
        html.push_str("</p>\n");
    }
    html.push_str("</header>\n");

    if report.exact_groups.is_empty() && report.visual_candidates.is_empty() {
        html.push_str("<div class=\"empty\"><strong>No duplicate image pins found.</strong><br>The scan completed without producing comparison candidates.</div>\n");
    }

    render_filters(&mut html, report, show_boards);

    for (index, group) in report.exact_groups.iter().enumerate() {
        let _ = writeln!(
            html,
            "<section class=\"match {}\"><div class=\"match-heading\"><div class=\"match-title\"><h2>Exact group {}</h2>{}</div><p>Byte-identical image files</p></div><div class=\"comparison\">",
            group.scope.css_class(),
            index + 1,
            scope_badge(group.scope, show_boards)
        );
        for item in &group.items {
            render_item(&mut html, item, show_boards);
        }
        html.push_str("</div></section>\n");
    }

    for (index, candidate) in report.visual_candidates.iter().enumerate() {
        let _ = writeln!(
            html,
            "<section class=\"match {}\"><div class=\"match-heading\"><div class=\"match-title\"><h2>Visual candidate {}</h2>{}</div><p>{}% similar · hash distance {}/64</p></div><div class=\"comparison\">",
            candidate.scope.css_class(),
            index + 1,
            scope_badge(candidate.scope, show_boards),
            candidate.similarity_percent,
            candidate.hash_distance
        );
        for item in &candidate.items {
            render_item(&mut html, item, show_boards);
        }
        html.push_str("</div></section>\n");
    }

    render_details(&mut html, report);
    html.push_str(
        "<footer>This temporary report loads images from Pinterest and remains available until your operating system cleans its temporary files.</footer>\n</main>\n</body>\n</html>\n",
    );
    html
}

/// Renders the scope filter tabs, or nothing when they would be dead controls.
///
/// The radio inputs are emitted as siblings of the match sections rather than
/// inside `.filters`, because the filtering CSS reaches the sections with `~`,
/// which only matches *later* siblings. The visible labels sit in `.filters` and
/// reach the inputs by `for=`.
fn render_filters(html: &mut String, report: &Report, show_boards: bool) {
    let (same, cross) = report.scope_counts();
    if !show_boards || same + cross == 0 {
        return;
    }

    for id in ["filter-all", "filter-same", "filter-cross"] {
        let checked = if id == "filter-all" { " checked" } else { "" };
        let _ = writeln!(
            html,
            "<input type=\"radio\" name=\"scope-filter\" id=\"{id}\" class=\"filter-input\"{checked}>"
        );
    }

    // A group of radios, not an ARIA tablist: real tab semantics need script to
    // manage focus and aria-selected, and announcing them without it would
    // describe behavior that is not there.
    html.push_str(
        "<div class=\"filters\" role=\"group\" aria-label=\"Filter matches by board scope\">\n",
    );
    for (id, label, count) in [
        ("filter-all", "All", same + cross),
        ("filter-same", "Same board", same),
        ("filter-cross", "Across boards", cross),
    ] {
        let _ = writeln!(
            html,
            "<label for=\"{id}\">{label} <span>{count}</span></label>"
        );
    }
    html.push_str("</div>\n");

    // Rust knows the counts, so an empty tab can explain itself without CSS
    // having to count anything.
    for (scope, count, message) in [
        (
            MatchScope::SameBoard,
            same,
            "No duplicates within a single board.",
        ),
        (
            MatchScope::CrossBoard,
            cross,
            "No duplicates spanning two boards.",
        ),
    ] {
        if count == 0 {
            let _ = writeln!(
                html,
                "<div class=\"empty filter-empty {}\">{message}</div>",
                scope.css_class()
            );
        }
    }
}

/// Badges a match as same-board or cross-board, or renders nothing when only
/// one board was scanned and the distinction cannot arise.
fn scope_badge(scope: MatchScope, show_boards: bool) -> String {
    if !show_boards {
        return String::new();
    }
    format!(
        "<span class=\"badge {}\">{}</span>",
        scope.css_class(),
        scope.html_label()
    )
}

fn render_item(html: &mut String, item: &ReportItem, show_board: bool) {
    let image_url = escape_html(&item.image_url);
    let pin_url = escape_html(&item.pin_url);
    let pin_id = escape_html(&item.pin_id);
    let board = match (show_board, &item.board) {
        (true, Some(board)) => format!("<span class=\"board\">{}</span>", escape_html(board)),
        _ => String::new(),
    };
    let _ = writeln!(
        html,
        "<article class=\"pin-card\"><a class=\"image-stage\" href=\"{image_url}\" target=\"_blank\" rel=\"noopener noreferrer\"><img src=\"{image_url}\" alt=\"Pinterest pin {pin_id}\" loading=\"lazy\" decoding=\"async\" referrerpolicy=\"no-referrer\"></a><div class=\"card-body\"><div class=\"card-top\"><span class=\"badge {}\">{}</span><span class=\"pin-id\">Pin {pin_id}</span></div>{board}<p class=\"dimensions\">{} × {} <span>· {}</span></p><div class=\"actions\"><a class=\"button primary\" href=\"{pin_url}\" target=\"_blank\" rel=\"noopener noreferrer\">Open pin</a><a class=\"button\" href=\"{image_url}\" target=\"_blank\" rel=\"noopener noreferrer\">Open image</a></div></div></article>",
        item.recommendation.css_class(),
        item.recommendation.label(),
        item.width,
        item.height,
        human_bytes(item.byte_size)
    );
}

fn render_details(html: &mut String, report: &Report) {
    if !report.skipped.is_empty() {
        let _ = writeln!(
            html,
            "<details><summary>{} skipped pin(s)</summary><ul>",
            report.skipped.len()
        );
        for skipped in &report.skipped {
            let id = skipped.pin_id.as_deref().unwrap_or("Unknown pin");
            let reason = escape_html(&skipped.reason);
            if let Some(url) = &skipped.pin_url {
                let _ = writeln!(
                    html,
                    "<li><a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\">{}</a>: {reason}</li>",
                    escape_html(url),
                    escape_html(id)
                );
            } else {
                let _ = writeln!(html, "<li>{}: {reason}</li>", escape_html(id));
            }
        }
        html.push_str("</ul></details>\n");
    }

    if !report.warnings.is_empty() {
        let _ = writeln!(
            html,
            "<details><summary>{} warning(s)</summary><ul>",
            report.warnings.len()
        );
        for warning in &report.warnings {
            let _ = writeln!(html, "<li>{}</li>", escape_html(warning));
        }
        html.push_str("</ul></details>\n");
    }
}

fn escape_html(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(character),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;
    use crate::pinterest::SkippedPin;
    use crate::report::{
        DuplicateGroup, Recommendation, ReportItem, ScannedBoard, Summary, VisualCandidate,
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
        assert!(html.contains("101.jpg?x=1&amp;y=2"));
        assert!(html.contains("Ideas &lt;script&gt;"));
        assert!(html.contains("<span>Pinterest total</span>"));
        assert!(!html.contains("<script>alert"));
        assert!(html.contains("video &lt;unsupported&gt;"));
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
