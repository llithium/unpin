use clap::{Parser, ValueEnum};
use std::path::PathBuf;

/// Find duplicate image pins in a Pinterest board or profile.
#[derive(Debug, Clone, Parser)]
#[command(name = "unpin", version, about)]
pub struct Cli {
    /// Board URL, USER/BOARD, profile URL, or username to inspect.
    #[arg(value_name = "TARGET")]
    pub target: String,

    /// Boards to scan for a profile target, by slug or name (repeatable, comma-separated).
    #[arg(
        long,
        value_name = "BOARD",
        value_delimiter = ',',
        conflicts_with = "interactive"
    )]
    pub boards: Vec<String>,

    /// Prompt to choose boards from a profile.
    #[arg(long)]
    pub interactive: bool,

    /// Report only duplicates whose pins are on the same board.
    #[arg(long, conflicts_with = "cross_board_only")]
    pub same_board_only: bool,

    /// Report only duplicates whose pins span different boards.
    #[arg(long, conflicts_with = "same_board_only")]
    pub cross_board_only: bool,

    /// Report format.
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub format: OutputFormat,

    /// Only report byte-identical images.
    #[arg(long)]
    pub exact_only: bool,

    /// Maximum 64-bit difference-hash distance for visual candidates.
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u8).range(0..=64))]
    pub similarity_threshold: u8,

    /// Do not create an HTML comparison report.
    #[arg(long)]
    pub no_visual: bool,

    /// Create the HTML report without opening it in a browser.
    #[arg(long)]
    pub no_open: bool,

    /// Do not show interactive progress.
    #[arg(long)]
    pub no_progress: bool,

    /// Do not use colors in text output.
    #[arg(long)]
    pub no_color: bool,

    /// Download and fingerprint every image instead of using the local cache.
    #[arg(long)]
    pub no_cache: bool,

    /// Import Pinterest cookies from a signed-in browser.
    #[arg(long, value_enum)]
    pub cookies_from_browser: Option<CookieBrowser>,

    /// Read Pinterest cookies from a Netscape-format cookies.txt file.
    #[arg(long, value_name = "FILE", conflicts_with = "cookies_from_browser")]
    pub cookies: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Default, Eq, PartialEq, ValueEnum)]
pub enum OutputFormat {
    #[default]
    Text,
    Json,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq, ValueEnum)]
pub enum CookieBrowser {
    Chrome,
    Chromium,
    Brave,
    Edge,
    Firefox,
    Arc,
    Vivaldi,
}

impl std::fmt::Display for CookieBrowser {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Chrome => "Chrome",
            Self::Chromium => "Chromium",
            Self::Brave => "Brave",
            Self::Edge => "Edge",
            Self::Firefox => "Firefox",
            Self::Arc => "Arc",
            Self::Vivaldi => "Vivaldi",
        };
        formatter.write_str(name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_root_command() {
        let cli = Cli::try_parse_from([
            "unpin",
            "https://www.pinterest.com/alice/interiors/",
            "--format",
            "json",
            "--exact-only",
            "--similarity-threshold",
            "8",
            "--no-visual",
            "--no-open",
            "--no-progress",
            "--no-color",
            "--no-cache",
            "--cookies-from-browser",
            "chrome",
        ])
        .unwrap();

        assert_eq!(cli.target, "https://www.pinterest.com/alice/interiors/");
        assert_eq!(cli.format, OutputFormat::Json);
        assert!(cli.exact_only);
        assert_eq!(cli.similarity_threshold, 8);
        assert!(cli.no_visual);
        assert!(cli.no_open);
        assert!(cli.no_progress);
        assert!(cli.no_color);
        assert!(cli.no_cache);
        assert_eq!(cli.cookies_from_browser, Some(CookieBrowser::Chrome));
    }

    #[test]
    fn rejects_similarity_threshold_above_64() {
        let result = Cli::try_parse_from([
            "unpin",
            "https://www.pinterest.com/alice/interiors/",
            "--similarity-threshold",
            "65",
        ]);

        assert!(result.is_err());
    }

    #[test]
    fn visual_progress_and_opening_are_enabled_by_default() {
        let cli =
            Cli::try_parse_from(["unpin", "https://www.pinterest.com/alice/interiors/"]).unwrap();

        assert!(!cli.no_visual);
        assert!(!cli.no_open);
        assert!(!cli.no_progress);
        assert!(!cli.no_color);
        assert!(!cli.no_cache);
        assert_eq!(cli.cookies_from_browser, None);
        assert_eq!(cli.cookies, None);
        assert!(cli.boards.is_empty());
        assert!(!cli.interactive);
        assert!(!cli.same_board_only);
        assert!(!cli.cross_board_only);
    }

    #[test]
    fn parses_username_targets_and_board_selection() {
        let cli =
            Cli::try_parse_from(["unpin", "alice", "--boards", "interiors,mood board"]).unwrap();
        assert_eq!(cli.target, "alice");
        assert_eq!(cli.boards, ["interiors", "mood board"]);

        // `--boards` is repeatable as well as comma-separated.
        let cli =
            Cli::try_parse_from(["unpin", "alice", "--boards", "a", "--boards", "b"]).unwrap();
        assert_eq!(cli.boards, ["a", "b"]);

        assert!(Cli::try_parse_from(["unpin", "alice", "--interactive"]).is_ok());
    }

    #[test]
    fn rejects_board_selection_flags_together() {
        let result =
            Cli::try_parse_from(["unpin", "alice", "--interactive", "--boards", "interiors"]);

        assert!(result.is_err());
    }

    #[test]
    fn parses_scope_filters_and_rejects_both_together() {
        let same = Cli::try_parse_from(["unpin", "alice", "--same-board-only"]).unwrap();
        assert!(same.same_board_only);
        assert!(!same.cross_board_only);

        let cross = Cli::try_parse_from(["unpin", "alice", "--cross-board-only"]).unwrap();
        assert!(cross.cross_board_only);
        assert!(!cross.same_board_only);

        assert!(
            Cli::try_parse_from(["unpin", "alice", "--same-board-only", "--cross-board-only"])
                .is_err()
        );
        assert!(Cli::try_parse_from(["unpin", "alice", "--all-boards"]).is_err());
    }

    #[test]
    fn parses_cookie_file_and_rejects_two_cookie_sources() {
        let cli =
            Cli::try_parse_from(["unpin", "alice/interiors", "--cookies", "cookies.txt"]).unwrap();
        assert_eq!(cli.cookies, Some(PathBuf::from("cookies.txt")));

        assert!(
            Cli::try_parse_from([
                "unpin",
                "alice",
                "--cookies",
                "cookies.txt",
                "--cookies-from-browser",
                "chrome"
            ])
            .is_err()
        );
    }
}
