use std::fmt::Write as FmtWrite;
use std::io::{self, Write as IoWrite};
use std::path::{Path, PathBuf};

use tempfile::Builder;
use thiserror::Error;

use crate::report::{Report, ReportItem, human_bytes};

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
.match-heading h2 { margin: 0; font-family: "Avenir Next Condensed", "Arial Narrow", sans-serif; font-size: clamp(1.55rem, 3vw, 2.35rem); font-weight: 800; letter-spacing: -.02em; text-transform: uppercase; }
.match-heading p { margin: 0; color: var(--muted); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: .76rem; text-align: right; text-transform: uppercase; }
.comparison { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 310px), 1fr)); gap: 18px; margin-top: 18px; }
.pin-card { min-width: 0; overflow: hidden; border: 1px solid #8293a2; border-radius: 3px; background: var(--card); box-shadow: 5px 5px 0 rgb(91 110 125 / 14%); }
.image-stage { position: relative; display: block; min-width: 0; overflow: hidden; background: #d9e2e8; line-height: 0; }
.image-stage::before { content: "IMAGE FIELD"; position: absolute; top: 7px; left: 9px; z-index: 1; padding: 2px 5px; color: #344655; background: rgb(249 251 252 / 88%); font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: .58rem; font-weight: 800; letter-spacing: .12em; }
.image-stage::after { content: ""; position: absolute; inset: 7px; pointer-events: none; background: linear-gradient(var(--signal), var(--signal)) left top / 16px 2px no-repeat, linear-gradient(var(--signal), var(--signal)) left top / 2px 16px no-repeat, linear-gradient(var(--signal), var(--signal)) right bottom / 16px 2px no-repeat, linear-gradient(var(--signal), var(--signal)) right bottom / 2px 16px no-repeat; }
.image-stage img { display: block; width: 100%; height: auto; }
.card-body { display: grid; gap: 13px; padding: 16px; }
.card-top { display: flex; align-items: center; justify-content: space-between; gap: 12px; }
.badge { display: inline-flex; align-items: center; min-height: 27px; padding: 4px 9px; border-radius: 999px; font-size: .72rem; font-weight: 850; letter-spacing: .08em; }
.badge.keep { color: var(--green); background: var(--green-soft); }
.badge.tie { color: var(--gold); background: var(--gold-soft); }
.badge.delete { color: var(--red); background: var(--red-soft); }
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
details { margin-top: 30px; padding: 16px 18px; border: 1px solid var(--line); border-radius: 3px; background: var(--card); }
summary { cursor: pointer; font-weight: 750; }
details ul { margin-bottom: 0; padding-left: 20px; color: var(--muted); line-height: 1.6; }
footer { margin-top: 52px; padding-top: 18px; border-top: 1px solid var(--line); color: var(--muted); font-size: .8rem; }
@media (prefers-reduced-motion: reduce) { .button:hover { transform: none; } }
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

    let show_boards = report.summary.boards.len() > 1;
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
    if report.summary.boards.len() > 1 {
        html.push_str("<p class=\"boards\">Scanned ");
        for (index, board) in report.summary.boards.iter().enumerate() {
            let separator = if index == 0 { "" } else { " · " };
            let _ = write!(
                html,
                "{separator}<a href=\"{}\" target=\"_blank\" rel=\"noopener noreferrer\">{}</a> ({} pins)",
                escape_html(&board.url),
                escape_html(&board.name),
                board.pins_found
            );
        }
        html.push_str("</p>\n");
    }
    html.push_str("</header>\n");

    if report.exact_groups.is_empty() && report.visual_candidates.is_empty() {
        html.push_str("<div class=\"empty\"><strong>No duplicate image pins found.</strong><br>The scan completed without producing comparison candidates.</div>\n");
    }

    for (index, group) in report.exact_groups.iter().enumerate() {
        let _ = writeln!(
            html,
            "<section class=\"match\"><div class=\"match-heading\"><h2>Exact group {}</h2><p>Byte-identical image files</p></div><div class=\"comparison\">",
            index + 1
        );
        for item in &group.items {
            render_item(&mut html, item, show_boards);
        }
        html.push_str("</div></section>\n");
    }

    for (index, candidate) in report.visual_candidates.iter().enumerate() {
        let _ = writeln!(
            html,
            "<section class=\"match\"><div class=\"match-heading\"><h2>Visual candidate {}</h2><p>{}% similar · hash distance {}/64</p></div><div class=\"comparison\">",
            index + 1,
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
                items: vec![item("101", Recommendation::Keep)],
            }],
            visual_candidates: vec![VisualCandidate {
                hash_distance: 2,
                similarity_percent: 96,
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
        assert!(html.contains("width: 100%; height: auto"));
        assert!(!html.contains("object-fit: contain"));
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
