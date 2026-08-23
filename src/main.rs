use std::io::IsTerminal;
use std::process::ExitCode;

use clap::Parser;
use unpin::cli::{Cli, OutputFormat};
use unpin::progress::{ProgressEvent, ProgressSink, TerminalProgress};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    // Installed before the bar hides the cursor, so a Ctrl-C in between cannot
    // exit with the cursor still hidden.
    unpin::progress::restore_cursor_on_interrupt();
    let progress = TerminalProgress::new(!cli.no_progress && std::io::stderr().is_terminal());
    let report = match unpin::run_with_api_root_and_progress(&cli, None, &progress).await {
        Ok(report) => report,
        Err(error) => {
            progress.emit(ProgressEvent::Failed);
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let report_to_open = if !cli.no_visual {
        progress.emit(ProgressEvent::ReportStarted);
        let report_path = match unpin::visual::create_temporary_report(&report) {
            Ok(path) => path,
            Err(error) => {
                progress.emit(ProgressEvent::Failed);
                eprintln!("error: {error}");
                return ExitCode::FAILURE;
            }
        };
        eprintln!("HTML report: {}", report_path.display());
        Some(report_path)
    } else {
        let rendered = match cli.format {
            OutputFormat::Text => report.render_text_with_color(
                !cli.no_color
                    && std::env::var_os("NO_COLOR").is_none()
                    && std::io::stdout().is_terminal(),
            ),
            OutputFormat::Json => match report.render_json() {
                Ok(json) => format!("{json}\n"),
                Err(error) => {
                    progress.emit(ProgressEvent::Failed);
                    eprintln!("error: failed to serialize report: {error}");
                    return ExitCode::FAILURE;
                }
            },
        };
        print!("{rendered}");
        None
    };
    progress.emit(ProgressEvent::Finished);

    if let Some(report_path) = report_to_open
        && !cli.no_open
        && let Err(error) = unpin::visual::open_report(&report_path)
    {
        eprintln!("warning: could not open the visual report in a browser: {error}");
    }

    ExitCode::SUCCESS
}
