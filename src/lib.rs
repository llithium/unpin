pub mod analysis;
pub mod auth;
pub mod cli;
pub mod pinterest;
pub mod progress;
pub mod report;
pub mod select;
pub mod visual;

use std::collections::HashSet;
use std::io::IsTerminal;

use thiserror::Error;
use url::Url;

use crate::cli::Cli;
use crate::pinterest::{BoardRef, PinterestClient, PinterestError, Target};
use crate::progress::{NoProgress, ProgressEvent, ProgressSink};
use crate::report::{Report, ScannedBoard, Summary};

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Auth(#[from] auth::AuthError),

    #[error(transparent)]
    Pinterest(#[from] PinterestError),

    #[error(transparent)]
    Analysis(#[from] analysis::AnalysisError),

    #[error(transparent)]
    Select(#[from] select::SelectError),

    #[error(
        "selecting boards for a profile requires an interactive terminal; pass --boards <BOARD,...> or --all-boards"
    )]
    BoardSelectionNotInteractive,

    #[error("--boards and --all-boards only apply to a username or profile URL")]
    BoardFlagsWithBoardUrl,

    /// Every selected board failed to fetch, so the per-board reasons are the
    /// only useful thing to report.
    #[error("no board could be scanned{}", listed(reasons))]
    AllBoardsFailed { reasons: Vec<String> },

    #[error("no analyzable static image pins were found{}", listed(reasons))]
    NoAnalyzablePins { reasons: Vec<String> },
}

/// Renders collected reasons as an indented list under an error message.
fn listed(reasons: &[String]) -> String {
    if reasons.is_empty() {
        return String::new();
    }
    format!("\n  {}", reasons.join("\n  "))
}

pub async fn run(cli: &Cli) -> Result<Report, AppError> {
    run_with_api_root_and_progress(cli, None, &NoProgress).await
}

pub async fn run_with_api_root(cli: &Cli, api_root: Option<Url>) -> Result<Report, AppError> {
    run_with_api_root_and_progress(cli, api_root, &NoProgress).await
}

pub async fn run_with_api_root_and_progress(
    cli: &Cli,
    api_root: Option<Url>,
    progress: &dyn ProgressSink,
) -> Result<Report, AppError> {
    let target = Target::parse(&cli.target)?;
    let cookies = if let Some(browser) = cli.cookies_from_browser {
        progress.emit(ProgressEvent::LoadingBrowserCookies {
            browser: browser.to_string(),
        });
        auth::load_pinterest_cookies(browser).await?
    } else {
        Vec::new()
    };
    let root = target.root().clone();
    let client = match api_root {
        Some(api_root) => PinterestClient::with_api_root_and_cookies(root, api_root, cookies)?,
        None => PinterestClient::with_cookies(root, cookies)?,
    };

    let (username, boards) = resolve_boards(cli, &target, &client, progress).await?;

    // Pins from every selected board are pooled into one analysis so that
    // duplicates spanning two boards are found as well.
    let mut seen_pin_ids = HashSet::new();
    let mut scanned_boards = Vec::new();
    let mut pins = Vec::new();
    let mut skipped = Vec::new();
    let mut warnings = Vec::new();
    let multiple = boards.len() > 1;

    for (index, board) in boards.iter().enumerate() {
        progress.emit(ProgressEvent::BoardStarted {
            name: board.name.clone(),
            current: index + 1,
            total: boards.len(),
        });
        let fetched = client
            .fetch_board_pins(board, &mut seen_pin_ids, progress)
            .await;
        let mut fetched = match fetched {
            Ok(fetched) => fetched,
            // One unreachable board should not throw away every other board's
            // results; a lone board has nothing to fall back on.
            Err(error) if multiple => {
                warnings.push(format!("{}: skipped, {error}", board.name));
                continue;
            }
            Err(error) => return Err(error.into()),
        };

        scanned_boards.push(ScannedBoard {
            name: fetched.board_name.clone(),
            url: board.url.clone(),
            pins_reported: fetched.pins_reported,
            pins_found: fetched.pins_found,
        });
        pins.append(&mut fetched.pins);
        skipped.append(&mut fetched.skipped);
        // Only a multi-board scan needs to say which board a warning is about.
        warnings.extend(fetched.warnings.into_iter().map(|warning| {
            if multiple {
                format!("{}: {warning}", board.name)
            } else {
                warning
            }
        }));
    }

    // Distinguish "nothing could be fetched" from "fetched, but nothing was
    // analyzable": only the first carries the per-board failure reasons, and
    // reporting it as the second would be both wrong and unactionable.
    if scanned_boards.is_empty() {
        return Err(AppError::AllBoardsFailed { reasons: warnings });
    }
    if pins.is_empty() {
        return Err(AppError::NoAnalyzablePins { reasons: warnings });
    }

    let mut analysis = analysis::analyze_pins_with_progress(
        pins,
        cli.exact_only,
        cli.similarity_threshold,
        progress,
    )
    .await?;
    skipped.append(&mut analysis.skipped);
    if analysis.analyzed == 0 {
        return Err(AppError::NoAnalyzablePins { reasons: warnings });
    }

    let pins_reported = scanned_boards
        .iter()
        .map(|board| board.pins_reported)
        .try_fold(0_usize, |total, reported| Some(total + reported?));
    let summary = Summary {
        username,
        pins_found: scanned_boards.iter().map(|board| board.pins_found).sum(),
        boards: scanned_boards,
        pins_reported,
        analyzed: analysis.analyzed,
        skipped: skipped.len(),
        exact_groups: analysis.exact_groups.len(),
        visual_candidates: analysis.visual_candidates.len(),
    };

    Ok(Report {
        summary,
        exact_groups: analysis.exact_groups,
        visual_candidates: analysis.visual_candidates,
        skipped,
        warnings,
        visual_report: None,
    })
}

/// Works out which boards to scan, prompting when a profile needs a choice.
async fn resolve_boards(
    cli: &Cli,
    target: &Target,
    client: &PinterestClient,
    progress: &dyn ProgressSink,
) -> Result<(Option<String>, Vec<BoardRef>), AppError> {
    let selecting = !cli.boards.is_empty() || cli.all_boards;

    let user = match target {
        Target::Board(board) => {
            if selecting {
                return Err(AppError::BoardFlagsWithBoardUrl);
            }
            let board = client.resolve_board(board, progress).await?;
            return Ok((None, vec![board]));
        }
        Target::User(user) => user,
    };

    // Decide how boards will be chosen before fetching them, so a run that
    // cannot prompt fails immediately instead of after a network round trip.
    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    if !cli.all_boards && cli.boards.is_empty() && !interactive {
        return Err(AppError::BoardSelectionNotInteractive);
    }

    let boards = client.fetch_user_boards(user, progress).await?;
    if boards.is_empty() {
        return Err(select::SelectError::NoBoards {
            username: user.username.clone(),
        }
        .into());
    }

    let selected = if cli.all_boards {
        (0..boards.len()).collect()
    } else if !cli.boards.is_empty() {
        select::resolve_requested(&cli.boards, &boards)?
    } else {
        progress.emit(ProgressEvent::SelectionStarted);
        let chosen = select::choose_boards(&user.username, &boards);
        progress.emit(ProgressEvent::SelectionFinished);
        chosen?
    };

    let boards = selected
        .into_iter()
        .map(|index| boards[index].clone())
        .collect();
    Ok((Some(user.username.clone()), boards))
}
