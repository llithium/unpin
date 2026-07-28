pub mod analysis;
pub mod auth;
pub mod cli;
pub mod pinterest;
pub mod progress;
pub mod report;
pub mod visual;

use thiserror::Error;
use url::Url;

use crate::cli::Cli;
use crate::pinterest::{BoardTarget, PinterestClient, PinterestError};
use crate::progress::{NoProgress, ProgressSink};
use crate::report::{Report, Summary};

#[derive(Debug, Error)]
pub enum AppError {
    #[error(transparent)]
    Auth(#[from] auth::AuthError),

    #[error(transparent)]
    Pinterest(#[from] PinterestError),

    #[error(transparent)]
    Analysis(#[from] analysis::AnalysisError),

    #[error("the board contained no analyzable static image pins")]
    NoAnalyzablePins,
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
    let target = BoardTarget::parse(&cli.board_url)?;
    let cookies = if let Some(browser) = cli.cookies_from_browser {
        progress.emit(crate::progress::ProgressEvent::LoadingBrowserCookies {
            browser: browser.to_string(),
        });
        auth::load_pinterest_cookies(browser).await?
    } else {
        Vec::new()
    };
    let client = match api_root {
        Some(root) => PinterestClient::with_api_root_and_cookies(target, root, cookies)?,
        None => PinterestClient::with_cookies(target, cookies)?,
    };
    let board = client.fetch_board_with_progress(progress).await?;
    if board.pins.is_empty() {
        return Err(AppError::NoAnalyzablePins);
    }

    let mut analysis = analysis::analyze_pins_with_progress(
        board.pins,
        cli.exact_only,
        cli.similarity_threshold,
        progress,
    )
    .await?;
    let mut skipped = board.skipped;
    skipped.append(&mut analysis.skipped);
    if analysis.analyzed == 0 {
        return Err(AppError::NoAnalyzablePins);
    }

    let summary = Summary {
        board_name: board.board_name,
        pins_reported: board.pins_reported,
        pins_found: board.pins_found,
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
        warnings: board.warnings,
        visual_report: None,
    })
}
