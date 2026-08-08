pub mod analysis;
pub mod auth;
pub mod cli;
pub mod pinterest;
pub mod progress;
pub mod report;
pub mod select;
pub mod visual;

use futures_util::stream::{self, StreamExt};
use std::collections::HashSet;
use std::io::IsTerminal;
use thiserror::Error;
use url::Url;

use crate::cli::Cli;
use crate::pinterest::{BoardRef, PinterestClient, PinterestError, Target};
use crate::progress::{NoProgress, ProgressEvent, ProgressSink};
use crate::report::{Report, ScannedBoard, Summary};

struct ResolvedSources {
    user: Option<crate::pinterest::UserTarget>,
    boards: Vec<BoardRef>,
    include_unorganized: bool,
    prefetched_unorganized: Option<Result<crate::pinterest::BoardPins, PinterestError>>,
}

/// Board feeds paginate sequentially, so overlapping whole boards is the only
/// way to shorten a multi-board scan. Kept modest because each board may also be
/// fanning out across its own sections.
const BOARD_FETCH_CONCURRENCY: usize = 6;

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

    #[error("--interactive requires an interactive terminal")]
    BoardSelectionNotInteractive,

    #[error("--boards and --interactive only apply to a username or profile URL")]
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
    let cookies = if let Some(path) = &cli.cookies {
        auth::load_pinterest_cookies_file(path)?
    } else if let Some(browser) = cli.cookies_from_browser {
        progress.emit(ProgressEvent::LoadingBrowserCookies {
            browser: browser.to_string(),
        });
        auth::load_pinterest_cookies(browser).await?
    } else {
        Vec::new()
    };
    let root = target.root().clone();
    // A custom API root is the deterministic test/integration path; never let
    // those synthetic media URLs leak into the user's normal cache.
    let use_default_cache = api_root.is_none() && !cli.no_cache;
    let client = match api_root {
        Some(api_root) => PinterestClient::with_api_root_and_cookies(root, api_root, cookies)?,
        None => PinterestClient::with_cookies(root, cookies)?,
    };

    let resolved = resolve_boards(cli, &target, &client, progress).await?;
    let user = resolved.user;
    let boards = resolved.boards;
    let username = user.as_ref().map(|user| user.username.clone());

    // Pins from every selected board are pooled into one analysis so that
    // duplicates spanning two boards are found as well.
    let mut seen_pin_ids = HashSet::new();
    let mut scanned_boards = Vec::new();
    let mut pins = Vec::new();
    let mut skipped = Vec::new();
    let mut warnings = Vec::new();
    // A profile feed is another independent source, even when the user has
    // only one visible board. Do not let one board failure prevent its
    // unorganized pins from being scanned.
    let multiple = boards.len() > 1 || user.is_some();

    let board_total = boards.len();
    let board_fetches = stream::iter(boards.into_iter().enumerate().map(|(index, board)| {
        let client = client.clone();
        async move {
            progress.emit(ProgressEvent::BoardStarted {
                name: board.name.clone(),
                current: index + 1,
                total: board_total,
            });
            let mut board_pin_ids = HashSet::new();
            let fetched = client
                .fetch_board_pins(&board, &mut board_pin_ids, progress)
                .await;
            (index, board, fetched)
        }
    }))
    .buffer_unordered(BOARD_FETCH_CONCURRENCY);
    futures_util::pin_mut!(board_fetches);
    let mut board_results = board_fetches.collect::<Vec<_>>().await;
    board_results.sort_by_key(|(index, _, _)| *index);

    for (_, board, fetched) in board_results {
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
        retain_unseen_board_items(&mut fetched, &mut seen_pin_ids, client.is_authenticated());

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

    // Pins saved straight to a profile are displayed by Pinterest as
    // "Unorganized ideas", not as a board. Include them in every profile scan
    // so they participate in the same duplicate analysis as board pins.
    if let Some(user) = user
        && resolved.include_unorganized
    {
        let fetched = match resolved.prefetched_unorganized {
            Some(fetched) => fetched,
            None => {
                client
                    .fetch_user_pins(&user, &mut seen_pin_ids, progress)
                    .await
            }
        };
        match fetched {
            Ok(mut fetched) => {
                scanned_boards.push(ScannedBoard {
                    name: fetched.board_name.clone(),
                    url: format!("https://www.pinterest.com/{}/", user.username),
                    pins_reported: fetched.pins_reported,
                    pins_found: fetched.pins_found,
                });
                pins.append(&mut fetched.pins);
                skipped.append(&mut fetched.skipped);
                warnings.extend(fetched.warnings);
            }
            // Board results remain useful if Pinterest denies just the profile
            // feed (for example, for a private profile viewed anonymously).
            // When every source fails, the common check below turns these
            // source-specific reasons into one actionable error.
            Err(error) => {
                warnings.push(format!("Unorganized ideas: skipped, {error}"));
            }
        }
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

    let cache_dir = use_default_cache
        .then(analysis::default_fingerprint_cache_dir)
        .flatten();
    let mut analysis = analysis::analyze_pins_with_progress_and_cache(
        pins,
        cli.exact_only,
        cli.similarity_threshold,
        progress,
        cache_dir,
    )
    .await?;
    skipped.append(&mut analysis.skipped);
    if analysis.analyzed == 0 {
        return Err(AppError::NoAnalyzablePins { reasons: warnings });
    }

    if cli.same_board_only {
        analysis
            .exact_groups
            .retain(|group| group.scope == crate::report::MatchScope::SameBoard);
        analysis
            .visual_candidates
            .retain(|candidate| candidate.scope == crate::report::MatchScope::SameBoard);
    } else if cli.cross_board_only {
        analysis
            .exact_groups
            .retain(|group| group.scope == crate::report::MatchScope::CrossBoard);
        analysis
            .visual_candidates
            .retain(|candidate| candidate.scope == crate::report::MatchScope::CrossBoard);
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

fn retain_unseen_board_items(
    fetched: &mut crate::pinterest::BoardPins,
    seen_pin_ids: &mut HashSet<String>,
    authenticated: bool,
) {
    fetched
        .pins
        .retain(|pin| seen_pin_ids.insert(pin.id.clone()));
    fetched.skipped.retain(|pin| {
        pin.pin_id
            .as_ref()
            .is_none_or(|id| seen_pin_ids.insert(id.clone()))
    });
    fetched.pins_found = fetched.pins.len() + fetched.skipped.len();
    fetched
        .warnings
        .retain(|warning| !warning.starts_with("Pinterest reports "));
    if let Some(warning) = crate::pinterest::incomplete_scan_warning(
        authenticated,
        fetched.pins_reported,
        fetched.pins_found,
    ) {
        fetched.warnings.push(warning);
    }
}

/// Works out which boards to scan, prompting when a profile needs a choice.
async fn resolve_boards(
    cli: &Cli,
    target: &Target,
    client: &PinterestClient,
    progress: &dyn ProgressSink,
) -> Result<ResolvedSources, AppError> {
    let selecting = !cli.boards.is_empty() || cli.interactive;

    let user = match target {
        Target::Board(board) => {
            if selecting {
                return Err(AppError::BoardFlagsWithBoardUrl);
            }
            let board = client.resolve_board(board, progress).await?;
            return Ok(ResolvedSources {
                user: None,
                boards: vec![board],
                include_unorganized: false,
                prefetched_unorganized: None,
            });
        }
        Target::User(user) => user,
    };

    // Decide how boards will be chosen before fetching them, so a run that
    // cannot prompt fails immediately instead of after a network round trip.
    if cli.interactive && !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return Err(AppError::BoardSelectionNotInteractive);
    }

    let boards = client.fetch_user_boards(user, progress).await?;
    if boards.is_empty() && !cli.boards.is_empty() {
        return Err(select::SelectError::NoBoards {
            username: user.username.clone(),
        }
        .into());
    }

    let mut include_unorganized = cli.boards.is_empty();
    let mut prefetched_unorganized = None;
    let selected = if !cli.boards.is_empty() {
        include_unorganized = false;
        select::resolve_requested(&cli.boards, &boards)?
    } else if cli.interactive {
        let fetched = client
            .fetch_user_pins(user, &mut HashSet::new(), progress)
            .await;
        let unorganized_count = fetched.as_ref().ok().map(|pins| pins.pins_found);
        let mut choices = boards.clone();
        choices.push(BoardRef {
            id: "__unorganized__".into(),
            name: "Unorganized ideas".into(),
            slug: "_quick_saves".into(),
            url: format!("https://www.pinterest.com/{}/", user.username),
            pins_reported: unorganized_count,
            section_count: 0,
            is_secret: false,
        });
        progress.emit(ProgressEvent::SelectionStarted);
        let chosen = select::choose_boards(&user.username, &choices);
        progress.emit(ProgressEvent::SelectionFinished);
        let chosen = chosen?;
        include_unorganized = chosen.contains(&boards.len());
        if include_unorganized {
            prefetched_unorganized = Some(fetched);
        }
        chosen
            .into_iter()
            .filter(|index| *index < boards.len())
            .collect()
    } else {
        (0..boards.len()).collect()
    };

    let boards = selected
        .into_iter()
        .map(|index| boards[index].clone())
        .collect();
    Ok(ResolvedSources {
        user: Some(user.clone()),
        boards,
        include_unorganized,
        prefetched_unorganized,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pinterest::{BoardPins, Pin, SkippedPin};

    #[test]
    fn concurrent_board_merge_deduplicates_in_original_order() {
        let mut seen = HashSet::from(["already-seen".to_owned()]);
        let mut fetched = BoardPins {
            board_name: "Second board".into(),
            pins_reported: Some(3),
            pins_found: 3,
            pins: vec![
                Pin {
                    id: "already-seen".into(),
                    media_url: "https://example.com/duplicate.jpg".into(),
                    metadata_width: None,
                    metadata_height: None,
                    board: Some("Second board".into()),
                },
                Pin {
                    id: "new-pin".into(),
                    media_url: "https://example.com/new.jpg".into(),
                    metadata_width: None,
                    metadata_height: None,
                    board: Some("Second board".into()),
                },
            ],
            skipped: vec![SkippedPin {
                pin_id: Some("already-seen".into()),
                pin_url: None,
                reason: "unsupported".into(),
                board: Some("Second board".into()),
            }],
            warnings: Vec::new(),
        };

        retain_unseen_board_items(&mut fetched, &mut seen, false);

        assert_eq!(fetched.pins.len(), 1);
        assert_eq!(fetched.pins[0].id, "new-pin");
        assert!(fetched.skipped.is_empty());
        assert_eq!(fetched.pins_found, 1);
        assert_eq!(fetched.warnings.len(), 1);
        assert!(fetched.warnings[0].contains("returned only 1 anonymously"));
    }
}
