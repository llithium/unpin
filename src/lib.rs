pub mod analysis;
pub mod auth;
pub mod cli;
pub mod pinterest;
pub mod progress;
pub mod report;
pub mod select;
pub mod visual;

mod image_fingerprint;
mod intake;
mod pinterest_api;

use std::collections::BTreeMap;

#[cfg(test)]
mod test_support {
    use std::sync::OnceLock;

    // The concurrency-boundary tests intentionally open dozens of delayed
    // connections at once. Keep those resource-heavy fixtures from running
    // together under libtest's default parallel scheduling.
    static HIGH_CONCURRENCY_TEST_LOCK: OnceLock<tokio::sync::Mutex<()>> = OnceLock::new();

    pub(crate) async fn high_concurrency_test_guard() -> tokio::sync::MutexGuard<'static, ()> {
        HIGH_CONCURRENCY_TEST_LOCK
            .get_or_init(|| tokio::sync::Mutex::new(()))
            .lock()
            .await
    }
}

use thiserror::Error;
use url::Url;

use crate::cli::Cli;
use crate::intake::{IntakeError, IntakeRequest, SourceOutcome, SourceSelection};
use crate::pinterest::{
    PinterestClient, PinterestError, SkippedPin, Target, aggregate_reported_pin_counts,
};
use crate::progress::{Lifecycle, NoProgress, Progress, ProgressStep, SetupTask};
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

    #[error("--interactive requires an interactive terminal")]
    BoardSelectionNotInteractive,

    #[error("--boards, --unorganized, and --interactive only apply to a username or profile URL")]
    BoardFlagsWithBoardUrl,

    /// Every selected source failed to fetch, so the per-source reasons are
    /// the only useful thing to report.
    #[error("no board could be scanned{}", listed(reasons))]
    AllBoardsFailed { reasons: Vec<String> },

    #[error("no analyzable static image pins were found{}", listed(reasons))]
    NoAnalyzablePins { reasons: Vec<String> },
}

fn map_intake_error(error: IntakeError) -> AppError {
    match error {
        IntakeError::Pinterest(error) => AppError::Pinterest(error),
        IntakeError::Select(error) => AppError::Select(error),
        IntakeError::BoardSelectionNotInteractive => AppError::BoardSelectionNotInteractive,
        IntakeError::BoardFlagsWithBoardUrl => AppError::BoardFlagsWithBoardUrl,
        IntakeError::AllSourcesFailed { failures } => AppError::AllBoardsFailed {
            reasons: failures
                .into_iter()
                .map(|failure| failure.warning())
                .collect(),
        },
    }
}

/// Renders collected reasons as an indented list under an error message.
fn listed(reasons: &[String]) -> String {
    if reasons.is_empty() {
        return String::new();
    }
    format!("\n  {}", reasons.join("\n  "))
}

/// Keeps the no-report error actionable when every pin was skipped before a
/// report could be built. Grouping avoids turning a large all-skipped scan into
/// one error line per pin while retaining the reasons a user can act on.
fn no_analyzable_reasons(mut warnings: Vec<String>, skipped: &[SkippedPin]) -> Vec<String> {
    let mut counts = BTreeMap::new();
    for pin in skipped {
        *counts.entry(pin.reason.as_str()).or_insert(0_usize) += 1;
    }

    let mut counts = counts.into_iter().collect::<Vec<_>>();
    counts.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    warnings.extend(counts.into_iter().map(|(reason, count)| {
        let pins = if count == 1 {
            "1 pin".to_owned()
        } else {
            format!("{count} pins")
        };
        format!("{pins} skipped: {reason}")
    }));
    warnings
}

pub async fn run(cli: &Cli) -> Result<Report, AppError> {
    run_with_api_root_and_progress(cli, None, &NoProgress).await
}

/// Runs with an optional custom API root for anonymous test and integration
/// scenarios. When imported cookies are present, the API root must stay on the
/// same HTTPS origin as the Pinterest target.
pub async fn run_with_api_root(cli: &Cli, api_root: Option<Url>) -> Result<Report, AppError> {
    run_with_api_root_and_progress(cli, api_root, &NoProgress).await
}

pub async fn run_with_api_root_and_progress(
    cli: &Cli,
    api_root: Option<Url>,
    progress: &dyn Progress,
) -> Result<Report, AppError> {
    let target = Target::parse(&cli.target)?;
    let cookies = if let Some(path) = &cli.cookies {
        auth::load_pinterest_scoped_cookies_file(path)?
    } else if let Some(browser) = cli.cookies_from_browser {
        let browser_name = browser.to_string();
        progress.step(ProgressStep::Setup {
            task: SetupTask::BrowserCookies {
                browser: browser_name.clone(),
            },
            lifecycle: Lifecycle::Started,
        });
        let cookies = auth::load_pinterest_scoped_cookies(browser).await?;
        progress.step(ProgressStep::Setup {
            task: SetupTask::BrowserCookies {
                browser: browser_name,
            },
            lifecycle: Lifecycle::Completed,
        });
        cookies
    } else {
        Vec::new()
    };
    let root = target.root().clone();
    // A custom API root is the deterministic test/integration path; never let
    // those synthetic media URLs leak into the user's normal cache.
    let use_default_cache = api_root.is_none() && !cli.no_cache;
    let client = match api_root {
        Some(api_root) => {
            PinterestClient::with_api_root_and_scoped_cookies(root, api_root, cookies)?
        }
        None => PinterestClient::with_api_root_and_scoped_cookies(root.clone(), root, cookies)?,
    };

    let selection = if cli.interactive {
        SourceSelection::Interactive
    } else if cli.boards.is_empty() && !cli.unorganized {
        SourceSelection::Default
    } else {
        SourceSelection::Requested {
            boards: cli.boards.clone(),
            include_unorganized: cli.unorganized,
        }
    };
    let intake = intake::collect(IntakeRequest { target, selection }, &client, progress)
        .await
        .map_err(map_intake_error)?;

    let username = intake.username;
    let mut scanned_boards = Vec::new();
    let pins = intake.pins;
    let mut skipped = intake.skipped;
    let mut warnings = Vec::new();
    for outcome in intake.sources {
        match outcome {
            SourceOutcome::Collected {
                source,
                warnings: source_warnings,
            } => {
                scanned_boards.push(ScannedBoard {
                    name: source.name,
                    url: source.url,
                    pins_reported: source.pins_reported,
                    pins_found: source.pins_found,
                });
                warnings.extend(source_warnings.into_iter().map(|warning| warning.render()));
            }
            SourceOutcome::Failed { source, error } => {
                warnings.push(format!("{}: skipped, {error}", source.name));
            }
        }
    }

    if pins.is_empty() {
        return Err(AppError::NoAnalyzablePins {
            reasons: no_analyzable_reasons(warnings, &skipped),
        });
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
        return Err(AppError::NoAnalyzablePins {
            reasons: no_analyzable_reasons(warnings, &skipped),
        });
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

    let pins_reported =
        aggregate_reported_pin_counts(scanned_boards.iter().map(|board| board.pins_reported));
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
