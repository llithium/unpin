use std::io::IsTerminal;
use std::process::ExitCode;

use clap::Parser;
use unpin::cli::{Cli, OutputFormat};
use unpin::progress::{Progress, ProgressStep, TerminalProgress};
use unpin::terminal_text::sanitize_terminal_text;

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Some(unpin::cli::Command::Render {
        input,
        output,
        no_open,
    }) = &cli.command
    {
        return match render_saved(input, output.as_deref(), *no_open) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("error: {}", sanitize_terminal_text(&error.to_string()));
                ExitCode::FAILURE
            }
        };
    }
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
        let report_path = match write_html(&report, cli.output.as_deref()) {
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
    if let Some(output) = cli_output {
        if let Some(path) = &cli.output {
            if let Err(error) = unpin::saved_report::write(path, output.as_bytes()) {
                progress.step(ProgressStep::Scan {
                    lifecycle: unpin::progress::Lifecycle::Failed,
                });
                eprintln!("error: {}", sanitize_terminal_text(&error.to_string()));
                return ExitCode::FAILURE;
            }
            eprintln!(
                "Report: {}",
                sanitize_terminal_text(&path.display().to_string())
            );
        } else {
            print!("{output}");
        }
    }
    progress.step(ProgressStep::Scan {
        lifecycle: unpin::progress::Lifecycle::Completed,
    });
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

fn write_html(
    report: &unpin::report::Report,
    output: Option<&std::path::Path>,
) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) = output {
        unpin::saved_report::write(path, unpin::visual::render_html(report).as_bytes())?;
        Ok(path.to_path_buf())
    } else {
        Ok(unpin::visual::create_temporary_report(report)?)
    }
}

fn render_saved(
    input: &std::path::Path,
    output: Option<&std::path::Path>,
    no_open: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let report = unpin::saved_report::read(input)?;
    let path = write_html(&report, output)?;
    eprintln!(
        "HTML report: {}",
        sanitize_terminal_text(&path.display().to_string())
    );
    if !no_open && let Err(error) = unpin::visual::open_report(&path) {
        eprintln!(
            "warning: could not open report: {}",
            sanitize_terminal_text(&error.to_string())
        );
    }
    Ok(())
}
