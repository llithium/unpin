//! Resolves a scan target, collects its selected sources, and normalizes their
//! outcomes before image analysis begins.

use futures_util::stream::{self, StreamExt};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use thiserror::Error;

use crate::pinterest::{
    BoardPins, BoardRef, Pin, PinterestClient, PinterestError, SkippedPin, Target, UserTarget,
};
use crate::progress::{Lifecycle, Progress, ProgressStep};
use crate::select;

/// Board feeds paginate sequentially, so overlapping whole boards is the only
/// way to shorten a multi-board scan. Individual requests still share the
/// client's API request limit.
const BOARD_FETCH_CONCURRENCY: usize = 12;

const UNORGANIZED_NAME: &str = "Unorganized ideas";

#[derive(Debug, Clone)]
pub(crate) enum SourceSelection {
    Default,
    Requested(Vec<String>),
    Interactive,
}

#[derive(Debug)]
pub(crate) struct IntakeRequest {
    pub(crate) target: Target,
    pub(crate) selection: SourceSelection,
}

#[derive(Debug)]
pub(crate) struct ScanIntakeResult {
    pub(crate) username: Option<String>,
    pub(crate) sources: Vec<SourceOutcome>,
    pub(crate) pins: Vec<Pin>,
    pub(crate) skipped: Vec<SkippedPin>,
}

#[derive(Debug)]
pub(crate) enum SourceOutcome {
    Collected {
        source: IntakeSource,
        warnings: Vec<SourceWarning>,
    },
    Failed {
        source: SourceIdentity,
        error: PinterestError,
    },
}

#[derive(Debug)]
pub(crate) struct IntakeSource {
    pub(crate) name: String,
    pub(crate) url: String,
    pub(crate) pins_reported: Option<usize>,
    pub(crate) pins_found: usize,
}

#[derive(Debug)]
pub(crate) struct SourceIdentity {
    pub(crate) name: String,
    #[allow(dead_code)]
    pub(crate) url: String,
}

#[derive(Debug)]
pub(crate) struct SourceWarning {
    source: Option<String>,
    message: String,
}

impl SourceWarning {
    pub(crate) fn render(self) -> String {
        match self.source {
            Some(source) => format!("{source}: {}", self.message),
            None => self.message,
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum IntakeError {
    #[error(transparent)]
    Pinterest(#[from] PinterestError),

    #[error(transparent)]
    Select(#[from] select::SelectError),

    #[error("--interactive requires an interactive terminal")]
    BoardSelectionNotInteractive,

    #[error("--boards and --interactive only apply to a username or profile URL")]
    BoardFlagsWithBoardUrl,

    #[error("all scan sources failed")]
    AllSourcesFailed { failures: Vec<SourceFailure> },
}

#[derive(Debug)]
pub(crate) struct SourceFailure {
    pub(crate) source: SourceIdentity,
    pub(crate) error: PinterestError,
}

impl SourceFailure {
    pub(crate) fn warning(self) -> String {
        format!("{}: skipped, {}", self.source.name, self.error)
    }
}

struct ResolvedSources {
    user: Option<UserTarget>,
    boards: Vec<BoardRef>,
    include_unorganized: bool,
    prefetched_unorganized: Option<Result<BoardPins, PinterestError>>,
}

pub(crate) async fn collect(
    request: IntakeRequest,
    client: &PinterestClient,
    progress: &dyn Progress,
) -> Result<ScanIntakeResult, IntakeError> {
    let ResolvedSources {
        user,
        boards,
        include_unorganized,
        prefetched_unorganized,
    } = resolve_sources(&request.target, request.selection, client, progress).await?;
    let username = user.as_ref().map(|user| user.username.clone());
    let multiple = boards.len() > 1 || user.is_some();

    // Keep source-level deduplication inside PinterestClient and perform the
    // cross-source merge here, where the selected source order is known.
    let mut seen_pin_ids = HashSet::new();
    let mut sources = Vec::new();
    let mut pins = Vec::new();
    let mut skipped = Vec::new();

    let board_total = boards.len();
    let board_completed = Arc::new(AtomicUsize::new(0));
    let board_fetches = stream::iter(boards.into_iter().enumerate().map(|(index, board)| {
        let client = client.clone();
        let board_completed = Arc::clone(&board_completed);
        async move {
            progress.step(ProgressStep::SourceCollection {
                name: board.name.clone(),
                current: index + 1,
                completed: board_completed.load(Ordering::Relaxed),
                total: board_total,
                lifecycle: Lifecycle::Started,
            });
            let fetched = client.collect_board_source(&board, progress).await;
            let completed = board_completed.fetch_add(1, Ordering::Relaxed) + 1;
            progress.step(ProgressStep::SourceCollection {
                name: board.name.clone(),
                current: index + 1,
                completed,
                total: board_total,
                lifecycle: Lifecycle::Completed,
            });
            (index, board, fetched)
        }
    }))
    .buffer_unordered(BOARD_FETCH_CONCURRENCY);

    // A profile's unorganized feed is independent of every board feed. Start
    // it with the board stream so a slow profile-level feed cannot become a
    // serial tail after all boards have finished.
    let user_for_unorganized = user.clone();
    let unorganized_fetch = async move {
        if !include_unorganized {
            return None;
        }
        let fetched = match prefetched_unorganized {
            Some(fetched) => fetched,
            None => {
                let user = user_for_unorganized.as_ref()?;
                client.collect_unorganized_source(user, progress).await
            }
        };
        Some(fetched)
    };
    let (mut board_results, fetched_unorganized) =
        tokio::join!(board_fetches.collect::<Vec<_>>(), unorganized_fetch);
    board_results.sort_by_key(|(index, _, _)| *index);

    for (_, board, fetched) in board_results {
        let mut fetched = match fetched {
            Ok(fetched) => fetched,
            // One unreachable board should not throw away every other board's
            // results; a lone board has no fallback source.
            Err(error) if multiple => {
                sources.push(SourceOutcome::Failed {
                    source: SourceIdentity {
                        name: board.name,
                        url: board.url,
                    },
                    error,
                });
                continue;
            }
            Err(error) => return Err(error.into()),
        };

        let (source, source_pins, source_skipped, source_warnings) = normalize_source(
            &mut fetched,
            &mut seen_pin_ids,
            client.is_authenticated(),
            multiple.then_some(board.name.clone()),
            board.url,
        );
        sources.push(SourceOutcome::Collected {
            source,
            warnings: source_warnings,
        });
        pins.extend(source_pins);
        skipped.extend(source_skipped);
    }

    // Pins saved straight to a profile are displayed by Pinterest as
    // "Unorganized ideas", not as a board. Include them in every profile scan
    // so they participate in the same duplicate analysis as board pins.
    if let Some(user) = user
        && include_unorganized
    {
        let fetched = match fetched_unorganized {
            Some(fetched) => fetched,
            None => client.collect_unorganized_source(&user, progress).await,
        };
        match fetched {
            Ok(mut fetched) => {
                let (source, source_pins, source_skipped, source_warnings) = normalize_source(
                    &mut fetched,
                    &mut seen_pin_ids,
                    client.is_authenticated(),
                    None,
                    format!("https://www.pinterest.com/{}/", user.username),
                );
                sources.push(SourceOutcome::Collected {
                    source,
                    warnings: source_warnings,
                });
                pins.extend(source_pins);
                skipped.extend(source_skipped);
            }
            Err(error) => {
                sources.push(SourceOutcome::Failed {
                    source: SourceIdentity {
                        name: UNORGANIZED_NAME.into(),
                        url: format!("https://www.pinterest.com/{}/", user.username),
                    },
                    error,
                });
            }
        }
    }

    if !sources
        .iter()
        .any(|source| matches!(source, SourceOutcome::Collected { .. }))
    {
        let failures = sources
            .into_iter()
            .filter_map(|source| match source {
                SourceOutcome::Failed { source, error } => Some(SourceFailure { source, error }),
                SourceOutcome::Collected { .. } => None,
            })
            .collect();
        return Err(IntakeError::AllSourcesFailed { failures });
    }

    Ok(ScanIntakeResult {
        username,
        sources,
        pins,
        skipped,
    })
}

fn normalize_source(
    fetched: &mut BoardPins,
    seen_pin_ids: &mut HashSet<String>,
    authenticated: bool,
    warning_source: Option<String>,
    url: String,
) -> (IntakeSource, Vec<Pin>, Vec<SkippedPin>, Vec<SourceWarning>) {
    retain_unseen_source(fetched, seen_pin_ids, authenticated);
    let source = IntakeSource {
        name: fetched.board_name.clone(),
        url,
        pins_reported: fetched.pins_reported,
        pins_found: fetched.pins_found,
    };
    let warnings = fetched
        .warnings
        .drain(..)
        .map(|message| SourceWarning {
            source: warning_source.clone(),
            message,
        })
        .collect();
    let pins = std::mem::take(&mut fetched.pins);
    let skipped = std::mem::take(&mut fetched.skipped);
    (source, pins, skipped, warnings)
}

fn retain_unseen_source(
    fetched: &mut BoardPins,
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

/// Works out which sources to scan, prompting when a profile needs a choice.
async fn resolve_sources(
    target: &Target,
    selection: SourceSelection,
    client: &PinterestClient,
    progress: &dyn Progress,
) -> Result<ResolvedSources, IntakeError> {
    let user = match target {
        Target::Board(board) => {
            if !matches!(&selection, SourceSelection::Default) {
                return Err(IntakeError::BoardFlagsWithBoardUrl);
            }
            let board = client.resolve_board_source(board, progress).await?;
            return Ok(ResolvedSources {
                user: None,
                boards: vec![board],
                include_unorganized: false,
                prefetched_unorganized: None,
            });
        }
        Target::User(user) => user,
    };

    if matches!(&selection, SourceSelection::Interactive)
        && !progress.interactive_terminal_available()
    {
        return Err(IntakeError::BoardSelectionNotInteractive);
    }

    let boards = client.list_profile_sources(user, progress).await?;
    if boards.is_empty() && matches!(&selection, SourceSelection::Requested(_)) {
        return Err(select::SelectError::NoBoards {
            username: user.username.clone(),
        }
        .into());
    }

    let (selected, include_unorganized, prefetched_unorganized) = match selection {
        SourceSelection::Requested(requested) => {
            (select::resolve_requested(&requested, &boards)?, false, None)
        }
        SourceSelection::Interactive => {
            let fetched = client.collect_unorganized_source(user, progress).await;
            let unorganized_count = fetched.as_ref().ok().map(|pins| pins.pins_found);
            let mut choices = boards.clone();
            choices.push(BoardRef {
                id: "__unorganized__".into(),
                name: UNORGANIZED_NAME.into(),
                slug: "_quick_saves".into(),
                url: format!("https://www.pinterest.com/{}/", user.username),
                pins_reported: unorganized_count,
                section_count: 0,
                is_secret: false,
            });
            progress.step(ProgressStep::SelectionHandoff {
                lifecycle: Lifecycle::Started,
            });
            let chosen = select::choose_boards(&user.username, &choices);
            progress.step(ProgressStep::SelectionHandoff {
                lifecycle: Lifecycle::Completed,
            });
            let chosen = chosen?;
            let include_unorganized = chosen.contains(&boards.len());
            let prefetched_unorganized = include_unorganized.then_some(fetched);
            (
                chosen
                    .into_iter()
                    .filter(|index| *index < boards.len())
                    .collect(),
                include_unorganized,
                prefetched_unorganized,
            )
        }
        SourceSelection::Default => ((0..boards.len()).collect(), true, None),
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

    #[tokio::test]
    async fn collect_returns_ordered_cross_source_outcomes() {
        use serde_json::json;
        use url::Url;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/resource/BoardsResource/get/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "resource_response": { "data": [
                    {
                        "id": "board-1",
                        "type": "board",
                        "name": "Interiors",
                        "url": "/alice/interiors/",
                        "pin_count": 1,
                        "section_count": 0,
                        "privacy": "public"
                    },
                    {
                        "id": "board-2",
                        "type": "board",
                        "name": "Mood board",
                        "url": "/alice/mood-board/",
                        "pin_count": 1,
                        "section_count": 0,
                        "privacy": "public"
                    }
                ]},
                "resource": { "options": { "bookmarks": ["-end-"] } }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/resource/BoardFeedResource/get/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "resource_response": { "data": [{
                    "id": "shared-pin",
                    "images": { "orig": {
                        "url": "https://example.com/shared.png",
                        "width": 20,
                        "height": 20
                    }}
                }]},
                "resource": { "options": { "bookmarks": ["-end-"] } }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/resource/UserPinsResource/get/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "resource_response": { "data": [] },
                "resource": { "options": { "bookmarks": ["-end-"] } }
            })))
            .mount(&server)
            .await;

        let target = Target::parse("alice").unwrap();
        let client = PinterestClient::with_api_root(
            target.root().clone(),
            Url::parse(&server.uri()).unwrap(),
        )
        .unwrap();
        let result = collect(
            IntakeRequest {
                target,
                selection: SourceSelection::Default,
            },
            &client,
            &crate::progress::NoProgress,
        )
        .await
        .unwrap();

        assert_eq!(result.username.as_deref(), Some("alice"));
        assert_eq!(result.pins.len(), 1);
        assert_eq!(result.pins[0].board.as_deref(), Some("Interiors"));
        assert_eq!(result.sources.len(), 3);
        let names = result
            .sources
            .iter()
            .map(|outcome| match outcome {
                SourceOutcome::Collected { source, .. } => source.name.as_str(),
                SourceOutcome::Failed { source, .. } => source.name.as_str(),
            })
            .collect::<Vec<_>>();
        assert_eq!(names, ["Interiors", "Mood board", "Unorganized ideas"]);
        match &result.sources[0] {
            SourceOutcome::Collected { source, .. } => assert_eq!(source.pins_found, 1),
            SourceOutcome::Failed { .. } => panic!("the first board should be collected"),
        }
        match &result.sources[1] {
            SourceOutcome::Collected { source, .. } => assert_eq!(source.pins_found, 0),
            SourceOutcome::Failed { .. } => panic!("the second board should be collected"),
        }
    }

    #[tokio::test]
    async fn profile_level_pins_start_with_board_fetches() {
        use std::time::{Duration, Instant};

        use serde_json::json;
        use url::Url;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/resource/BoardsResource/get/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "resource_response": { "data": [{
                    "id": "board-1",
                    "type": "board",
                    "name": "Interiors",
                    "url": "/alice/interiors/",
                    "pin_count": 0,
                    "section_count": 0,
                    "privacy": "public"
                }]},
                "resource": { "options": { "bookmarks": ["-end-"] } }
            })))
            .mount(&server)
            .await;
        let delayed_empty = || {
            ResponseTemplate::new(200)
                .set_body_json(json!({
                    "resource_response": { "data": [] },
                    "resource": { "options": { "bookmarks": ["-end-"] } }
                }))
                .set_delay(Duration::from_millis(500))
        };
        Mock::given(method("GET"))
            .and(path("/resource/BoardFeedResource/get/"))
            .respond_with(delayed_empty())
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/resource/UserPinsResource/get/"))
            .respond_with(delayed_empty())
            .expect(1)
            .mount(&server)
            .await;

        let target = Target::parse("alice").unwrap();
        let client = PinterestClient::with_api_root(
            target.root().clone(),
            Url::parse(&server.uri()).unwrap(),
        )
        .unwrap();
        let scan = tokio::spawn(async move {
            collect(
                IntakeRequest {
                    target,
                    selection: SourceSelection::Default,
                },
                &client,
                &crate::progress::NoProgress,
            )
            .await
        });

        let deadline = Instant::now() + Duration::from_millis(250);
        loop {
            let paths = server
                .received_requests()
                .await
                .unwrap()
                .into_iter()
                .map(|request| request.url.path().to_owned())
                .collect::<HashSet<_>>();
            if paths.contains("/resource/BoardFeedResource/get/")
                && paths.contains("/resource/UserPinsResource/get/")
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "profile-level pins did not start with the board feed"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }

        assert!(scan.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn board_fetches_are_bounded_by_the_board_concurrency_limit() {
        use std::time::{Duration, Instant};

        use clap::Parser;
        use serde_json::json;
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let _test_guard = crate::test_support::high_concurrency_test_guard().await;
        let server = MockServer::start().await;
        let total = BOARD_FETCH_CONCURRENCY * 2 + 1;
        let boards = (0..total)
            .map(|index| {
                json!({
                    "id": format!("board-{index}"),
                    "type": "board",
                    "name": format!("Board {index}"),
                    "url": format!("/alice/board-{index}/"),
                    "pin_count": 0,
                    "section_count": 0,
                    "privacy": "public"
                })
            })
            .collect::<Vec<_>>();
        Mock::given(method("GET"))
            .and(path("/resource/BoardsResource/get/"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "resource_response": { "data": boards },
                "resource": { "options": { "bookmarks": ["-end-"] } }
            })))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/resource/BoardFeedResource/get/"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(json!({
                        "resource_response": { "data": [] },
                        "resource": { "options": { "bookmarks": ["-end-"] } }
                    }))
                    .set_delay(Duration::from_millis(500)),
            )
            .expect(total as u64)
            .mount(&server)
            .await;

        let cli = crate::cli::Cli::try_parse_from(["unpin", "alice"]).unwrap();
        let api_root = url::Url::parse(&server.uri()).unwrap();
        let scan = tokio::spawn(async move {
            let progress = crate::progress::NoProgress;
            crate::run_with_api_root_and_progress(&cli, Some(api_root), &progress).await
        });

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let received = server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .filter(|request| request.url.path() == "/resource/BoardFeedResource/get/")
                .count();
            if received >= BOARD_FETCH_CONCURRENCY {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the first board-fetch wave did not start"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        let received = server
            .received_requests()
            .await
            .unwrap()
            .iter()
            .filter(|request| request.url.path() == "/resource/BoardFeedResource/get/")
            .count();
        assert_eq!(
            received, BOARD_FETCH_CONCURRENCY,
            "a board fetch beyond the configured limit started before a response completed"
        );

        assert!(scan.await.unwrap().is_err());
    }
}
