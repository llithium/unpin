//! Choosing which of a profile's boards to scan.

use inquire::MultiSelect;
use thiserror::Error;

use crate::pinterest::BoardRef;

#[derive(Debug, Error)]
pub enum SelectError {
    #[error("{username} has no boards that are visible to this scan")]
    NoBoards { username: String },

    #[error("no boards were selected")]
    NothingSelected,

    #[error("no board matched {requested:?}; available boards are: {available}")]
    UnknownBoard {
        requested: String,
        available: String,
    },

    #[error("could not read the board selection")]
    Prompt(#[source] inquire::InquireError),
}

/// The picker's first row selects every board at once.
const ALL_BOARDS_ROW: usize = 0;

/// Prompts for boards interactively. Requires a terminal.
pub fn choose_boards(username: &str, boards: &[BoardRef]) -> Result<Vec<usize>, SelectError> {
    if boards.is_empty() {
        return Err(SelectError::NoBoards {
            username: username.to_owned(),
        });
    }

    let labels = picker_labels(boards, terminal_width());

    // `raw_prompt` keeps each choice's original row index, which is what maps
    // back onto `boards` once the leading "all boards" row is accounted for.
    let rows = MultiSelect::new(&format!("Select boards to scan for {username}:"), labels)
        .with_help_message(
            "↑↓ move · space toggles · → all · ← none · type to filter · enter confirms",
        )
        .raw_prompt()
        .map_err(SelectError::Prompt)?
        .into_iter()
        .map(|choice| choice.index)
        .collect::<Vec<_>>();

    let selected = boards_from_rows(&rows, boards.len());
    if selected.is_empty() {
        return Err(SelectError::NothingSelected);
    }
    Ok(selected)
}

/// Maps picked rows back to board indices, accounting for the "all" row.
fn boards_from_rows(rows: &[usize], board_count: usize) -> Vec<usize> {
    if rows.contains(&ALL_BOARDS_ROW) {
        return (0..board_count).collect();
    }
    rows.iter().filter_map(|row| row.checked_sub(1)).collect()
}

/// Blank columns between a board name and its pin count.
const LABEL_GAP: usize = 2;
/// Room inquire's own `> [ ] ` row prefix takes before our label starts.
const ROW_PREFIX: usize = 6;
/// Never squeeze names below this, even in a very narrow terminal.
const MIN_NAME_WIDTH: usize = 12;

fn terminal_width() -> usize {
    // inquire draws to stderr, so that is the width that matters.
    console::Term::stderr().size().1 as usize
}

/// Builds the picker rows, sized so each one fits on a single line.
///
/// Names are padded only as far as the longest one actually needs, and are
/// truncated when even that will not fit the terminal.
fn picker_labels(boards: &[BoardRef], terminal_width: usize) -> Vec<String> {
    let total = boards
        .iter()
        .map(|board| board.pins_reported)
        .try_fold(0_usize, |total, reported| Some(total + reported?));

    let mut rows = Vec::with_capacity(boards.len() + 1);
    rows.push((format!("All {} boards", boards.len()), pin_count(total)));
    rows.extend(boards.iter().map(|board| {
        let secret = if board.is_secret { "  (secret)" } else { "" };
        (
            board.name.clone(),
            format!("{}{secret}", pin_count(board.pins_reported)),
        )
    }));

    let width = |value: &str| value.chars().count();
    let widest_name = rows.iter().map(|(name, _)| width(name)).max().unwrap_or(0);
    let widest_count = rows
        .iter()
        .map(|(_, count)| width(count))
        .max()
        .unwrap_or(0);
    let budget = terminal_width
        .saturating_sub(ROW_PREFIX + widest_count + LABEL_GAP)
        .max(MIN_NAME_WIDTH);
    let name_width = widest_name.min(budget);

    rows.iter()
        .map(|(name, count)| {
            format!(
                "{:<name_width$}{}{count}",
                truncate(name, name_width),
                " ".repeat(LABEL_GAP)
            )
        })
        .collect()
}

fn truncate(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_owned();
    }
    value
        .chars()
        .take(width.saturating_sub(1))
        .chain(std::iter::once('…'))
        .collect()
}

fn pin_count(pins: Option<usize>) -> String {
    match pins {
        Some(1) => "1 pin".to_owned(),
        Some(count) => format!("{count} pins"),
        None => "? pins".to_owned(),
    }
}

/// Resolves `--boards` values against the profile's board list.
///
/// Each value matches a slug first, then a board name, case-insensitively.
pub fn resolve_requested(
    requested: &[String],
    boards: &[BoardRef],
) -> Result<Vec<usize>, SelectError> {
    let mut selected = Vec::new();

    for value in requested {
        let wanted = value.trim().to_lowercase();
        let found = boards
            .iter()
            .position(|board| board.slug.to_lowercase() == wanted)
            .or_else(|| {
                boards
                    .iter()
                    .position(|board| board.name.to_lowercase() == wanted)
            })
            .ok_or_else(|| SelectError::UnknownBoard {
                requested: value.clone(),
                available: available_boards(boards),
            })?;

        if !selected.contains(&found) {
            selected.push(found);
        }
    }

    if selected.is_empty() {
        return Err(SelectError::NothingSelected);
    }
    Ok(selected)
}

fn available_boards(boards: &[BoardRef]) -> String {
    if boards.is_empty() {
        return "none".to_owned();
    }
    boards
        .iter()
        .map(|board| board.slug.clone())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn board(name: &str, slug: &str) -> BoardRef {
        BoardRef {
            id: format!("id-{slug}"),
            name: name.into(),
            slug: slug.into(),
            url: format!("https://www.pinterest.com/alice/{slug}/"),
            pins_reported: Some(10),
            section_count: 0,
            is_secret: false,
        }
    }

    fn boards() -> Vec<BoardRef> {
        vec![
            board("Interiors", "interiors"),
            board("Mood board", "mood-board"),
        ]
    }

    #[test]
    fn matches_slugs_then_names_case_insensitively() {
        let boards = boards();
        assert_eq!(
            resolve_requested(&["interiors".into()], &boards).unwrap(),
            [0]
        );
        assert_eq!(
            resolve_requested(&["MOOD-BOARD".into()], &boards).unwrap(),
            [1]
        );
        assert_eq!(
            resolve_requested(&["Mood Board".into()], &boards).unwrap(),
            [1]
        );
        assert_eq!(
            resolve_requested(&["  interiors  ".into()], &boards).unwrap(),
            [0]
        );
    }

    #[test]
    fn keeps_request_order_and_ignores_repeats() {
        let boards = boards();
        let selected = resolve_requested(
            &["mood-board".into(), "interiors".into(), "Mood board".into()],
            &boards,
        )
        .unwrap();

        assert_eq!(selected, [1, 0]);
    }

    #[test]
    fn unknown_board_lists_the_available_slugs() {
        let error = resolve_requested(&["recipes".into()], &boards()).unwrap_err();
        let message = error.to_string();

        assert!(message.contains("recipes"));
        assert!(message.contains("interiors"));
        assert!(message.contains("mood-board"));
    }

    #[test]
    fn empty_request_is_an_error() {
        assert!(matches!(
            resolve_requested(&[], &boards()).unwrap_err(),
            SelectError::NothingSelected
        ));
    }

    #[test]
    fn first_picker_row_selects_every_board() {
        // Row 0 is "all boards"; later rows are offset by one.
        assert_eq!(boards_from_rows(&[0], 3), [0, 1, 2]);
        assert_eq!(boards_from_rows(&[1], 3), [0]);
        assert_eq!(boards_from_rows(&[1, 3], 3), [0, 2]);
        assert_eq!(boards_from_rows(&[3, 1], 3), [2, 0]);

        // Ticking "all" alongside individual boards still means all of them.
        assert_eq!(boards_from_rows(&[2, 0], 3), [0, 1, 2]);

        assert!(boards_from_rows(&[], 3).is_empty());
    }

    #[test]
    fn all_boards_row_totals_the_pin_counts() {
        let mut boards = boards();
        let labels = picker_labels(&boards, 80);
        assert!(labels[0].contains("All 2 boards"));
        assert!(labels[0].contains("20 pins"));

        // An unknown count anywhere makes the total unknown rather than wrong.
        boards[1].pins_reported = None;
        assert!(picker_labels(&boards, 80)[0].contains("? pins"));
    }

    #[test]
    fn labels_show_pin_counts_and_secrecy() {
        let mut boards = boards();
        boards[1].is_secret = true;
        boards[1].pins_reported = Some(1);
        let labels = picker_labels(&boards, 80);

        assert!(labels[2].contains("1 pin"));
        assert!(labels[2].contains("(secret)"));
        assert!(!labels[1].contains("(secret)"));
    }

    #[test]
    fn rows_fit_on_one_line_in_a_narrow_terminal() {
        let mut boards = boards();
        boards.push(board("A board with a very long name indeed", "long"));

        // The real case: a 40-column terminal must not wrap any row.
        for width in [40, 60, 80, 120] {
            for label in picker_labels(&boards, width) {
                let rendered = ROW_PREFIX + label.chars().count();
                assert!(
                    rendered <= width,
                    "width {width}: {rendered} cols in {label:?}"
                );
            }
        }
    }

    #[test]
    fn names_are_padded_only_as_far_as_the_longest_needs() {
        // With room to spare, padding tracks the longest name, not a constant.
        let labels = picker_labels(&boards(), 200);
        let widest = ["All 2 boards", "Interiors", "Mood board"]
            .iter()
            .map(|name| name.chars().count())
            .max()
            .unwrap();

        for label in &labels {
            assert_eq!(label.chars().count(), widest + LABEL_GAP + "10 pins".len());
        }
        assert!(!labels[1].contains("…"));
    }

    #[test]
    fn overlong_names_are_truncated_rather_than_wrapped() {
        assert_eq!(truncate("Interiors", 9), "Interiors");
        assert_eq!(truncate("Interiors", 5), "Inte…");
        // Truncation counts characters, not bytes.
        assert_eq!(truncate("Can’t stop cross-stitching", 6), "Can’t…");
    }
}
