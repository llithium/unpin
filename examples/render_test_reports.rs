//! Render a deterministic multi-board report for browser-level UI tests.

use std::fs;
use std::path::PathBuf;

use clap::{Parser, ValueEnum};
use unpin::report::{
    DuplicateGroup, MatchScope, Recommendation, Report, ReportItem, ScannedBoard, Summary,
    VisualCandidate,
};
use unpin::visual::render_html;

#[derive(Debug, Parser)]
struct Args {
    output: PathBuf,
    #[arg(long, value_enum, default_value_t = Scenario::Mixed)]
    scenario: Scenario,
}

#[derive(Clone, Debug, Default, ValueEnum)]
enum Scenario {
    #[default]
    Mixed,
    Empty,
    LongContent,
}

fn item(
    id: &str,
    board: &str,
    source_id: &str,
    width: u32,
    height: u32,
    byte_size: u64,
    recommendation: Recommendation,
) -> ReportItem {
    ReportItem {
        pin_id: id.into(),
        pin_url: format!("https://www.pinterest.com/pin/{id}/"),
        board: Some(board.into()),
        source_id: Some(source_id.into()),
        image_url: format!("https://images.example.test/{id}.jpg"),
        width,
        height,
        byte_size,
        recommendation,
    }
}

fn fixture() -> Report {
    let mut exact_second = item(
        "102",
        "Ideas",
        "board-a",
        1200,
        800,
        2048,
        Recommendation::Tie,
    );
    exact_second.image_url = "https://images.example.test/101.jpg".into();
    Report {
        summary: Summary {
            username: Some("alice".into()),
            boards: vec![
                ScannedBoard {
                    name: "Ideas".into(),
                    url: "https://www.pinterest.com/alice/ideas/".into(),
                    pins_reported: Some(3),
                    pins_found: 3,
                },
                ScannedBoard {
                    name: "Mood board".into(),
                    url: "https://www.pinterest.com/alice/mood-board/".into(),
                    pins_reported: Some(3),
                    pins_found: 3,
                },
            ],
            pins_reported: Some(6),
            pins_found: 6,
            analyzed: 6,
            skipped: 0,
            exact_groups: 2,
            visual_candidates: 1,
        },
        exact_groups: vec![
            DuplicateGroup {
                scope: MatchScope::SameBoard,
                items: vec![
                    item(
                        "101",
                        "Ideas",
                        "board-a",
                        1200,
                        800,
                        2048,
                        Recommendation::Tie,
                    ),
                    exact_second,
                ],
            },
            DuplicateGroup {
                scope: MatchScope::CrossBoard,
                items: vec![
                    item(
                        "201",
                        "Ideas",
                        "board-a",
                        900,
                        1200,
                        1024,
                        Recommendation::Tie,
                    ),
                    item(
                        "202",
                        "Mood board",
                        "board-b",
                        900,
                        1200,
                        1024,
                        Recommendation::Tie,
                    ),
                ],
            },
        ],
        visual_candidates: vec![VisualCandidate {
            hash_distance: 4,
            similarity_percent: 98,
            scope: MatchScope::SameBoard,
            items: [
                item(
                    "301",
                    "Mood board",
                    "board-b",
                    800,
                    800,
                    4096,
                    Recommendation::Keep,
                ),
                item(
                    "302",
                    "Mood board",
                    "board-b",
                    798,
                    800,
                    3000,
                    Recommendation::DeleteCandidate,
                ),
            ],
        }],
        skipped: vec![],
        warnings: vec![],
    }
}

fn main() {
    let args = Args::parse();
    let mut report = fixture();
    match args.scenario {
        Scenario::Mixed => {}
        Scenario::Empty => {
            report.exact_groups.clear();
            report.visual_candidates.clear();
            report.summary.exact_groups = 0;
            report.summary.visual_candidates = 0;
            report.warnings.push(
                "One board could not be scanned. Results cover the available pins only.".into(),
            );
        }
        Scenario::LongContent => {
            report.summary.username = Some("a-very-long-profile-name-with-many-characters".into());
            for group in &mut report.exact_groups {
                for item in &mut group.items {
                    item.board = Some(
                        "A very long board name with art references and illustration inspiration"
                            .into(),
                    );
                    item.pin_id = format!("12345678901234567{}", item.pin_id);
                }
            }
            let mut extra = report.exact_groups[0].items[0].clone();
            extra.pin_id = "12345678901234567999".into();
            report.exact_groups[0].items.push(extra);
        }
    }
    fs::write(args.output, render_html(&report)).expect("write visual fixture");
}
