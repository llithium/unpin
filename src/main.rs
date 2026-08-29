use std::io::IsTerminal;
use std::process::ExitCode;

use clap::Parser;
use unpin::cli::{Cli, OutputFormat};
use unpin::progress::{Progress, ProgressStep, TerminalProgress};
use unpin::terminal_text::sanitize_terminal_text;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    // Installed before the bar hides the cursor, so a Ctrl-C in between cannot
    // exit with the cursor still hidden.
    unpin::progress::restore_cursor_on_interrupt();
    let progress_visible = !cli.no_progress && std::io::stderr().is_terminal();
    let progress = TerminalProgress::new(progress_visible);
    let report = match unpin::run_with_api_root_and_progress(&cli, None, &progress).await {
        Ok(report) => report,
        Err(error) => {
            progress.step(ProgressStep::Scan {
                lifecycle: unpin::progress::Lifecycle::Failed,
            });
            eprintln!("error: {}", sanitize_terminal_text(&error.to_string()));
            return ExitCode::FAILURE;
        }
    };
    let (report_to_open, cli_output) = if !cli.no_visual {
        progress.step(ProgressStep::ReportCreation {
            path: None,
            lifecycle: unpin::progress::Lifecycle::Started,
        });
        let report_path = match unpin::visual::create_temporary_report(&report) {
            Ok(path) => path,
            Err(error) => {
                progress.step(ProgressStep::Scan {
                    lifecycle: unpin::progress::Lifecycle::Failed,
                });
                eprintln!("error: {}", sanitize_terminal_text(&error.to_string()));
                return ExitCode::FAILURE;
            }
        };
        progress.step(ProgressStep::ReportCreation {
            path: Some(report_path.display().to_string()),
            lifecycle: unpin::progress::Lifecycle::Completed,
        });
        (Some(report_path), None)
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
                    progress.step(ProgressStep::Scan {
                        lifecycle: unpin::progress::Lifecycle::Failed,
                    });
                    eprintln!(
                        "error: failed to serialize report: {}",
                        sanitize_terminal_text(&error.to_string())
                    );
                    return ExitCode::FAILURE;
                }
            },
        };
        (None, Some(rendered))
    };
    progress.step(ProgressStep::Scan {
        lifecycle: unpin::progress::Lifecycle::Completed,
    });

    if let Some(output) = cli_output {
        print!("{output}");
    }
    if !progress_visible && let Some(report_path) = &report_to_open {
        eprintln!(
            "HTML report: {}",
            sanitize_terminal_text(&report_path.display().to_string())
        );
    }

    if let Some(report_path) = report_to_open
        && !cli.no_open
        && let Err(error) = unpin::visual::open_report(&report_path)
    {
        eprintln!(
            "warning: could not open the visual report in a browser: {}",
            sanitize_terminal_text(&error.to_string())
        );
    }

    ExitCode::SUCCESS
}
