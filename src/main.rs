use std::io::IsTerminal;
use std::process::ExitCode;

use clap::Parser;
use unpin::cli::{Cli, OutputFormat};
use unpin::progress::{ProgressEvent, ProgressSink, TerminalProgress};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let progress = TerminalProgress::new(!cli.no_progress && std::io::stderr().is_terminal());
    let mut report = match unpin::run_with_api_root_and_progress(&cli, None, &progress).await {
        Ok(report) => report,
        Err(error) => {
            progress.emit(ProgressEvent::Failed);
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut report_to_open = None;

    if !cli.no_visual {
        progress.emit(ProgressEvent::ReportStarted);
        let report_path = match unpin::visual::create_temporary_report(&report) {
            Ok(path) => path,
            Err(error) => {
                progress.emit(ProgressEvent::Failed);
                eprintln!("error: {error}");
                return ExitCode::FAILURE;
            }
        };
        report.visual_report = Some(report_path.to_string_lossy().into_owned());
        if !cli.no_open {
            report_to_open = Some(report_path);
        }
    }

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
    progress.emit(ProgressEvent::Finished);

    if let Some(report_path) = report_to_open
        && let Err(error) = unpin::visual::open_report(&report_path)
    {
        eprintln!("warning: could not open the visual report in a browser: {error}");
    }

    print!("{rendered}");
    ExitCode::SUCCESS
}
