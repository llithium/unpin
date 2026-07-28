use std::collections::BTreeMap;
use std::fmt::Write;

use console::Style;
use serde::{Deserialize, Serialize};

use crate::pinterest::SkippedPin;

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Recommendation {
    Keep,
    Tie,
    DeleteCandidate,
}

impl Recommendation {
    pub(crate) fn label(&self) -> &'static str {
        match self {
            Self::Keep => "KEEP",
            Self::Tie => "TIE",
            Self::DeleteCandidate => "DELETE?",
        }
    }

    pub(crate) fn css_class(&self) -> &'static str {
        match self {
            Self::Keep => "keep",
            Self::Tie => "tie",
            Self::DeleteCandidate => "delete",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct ReportItem {
    pub pin_id: String,
    pub pin_url: String,
    pub image_url: String,
    pub width: u32,
    pub height: u32,
    pub byte_size: u64,
    pub recommendation: Recommendation,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct DuplicateGroup {
    pub items: Vec<ReportItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct VisualCandidate {
    pub hash_distance: u8,
    pub similarity_percent: u8,
    pub items: [ReportItem; 2],
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct Summary {
    pub board_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pins_reported: Option<usize>,
    pub pins_found: usize,
    pub analyzed: usize,
    pub skipped: usize,
    pub exact_groups: usize,
    pub visual_candidates: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, Eq, PartialEq)]
pub struct Report {
    pub summary: Summary,
    pub exact_groups: Vec<DuplicateGroup>,
    pub visual_candidates: Vec<VisualCandidate>,
    pub skipped: Vec<SkippedPin>,
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visual_report: Option<String>,
}

impl Report {
    pub fn render_text(&self) -> String {
        self.render_text_with_color(false)
    }

    pub fn render_text_with_color(&self, color: bool) -> String {
        let mut output = String::new();
        let theme = TextTheme { color };
        let summary = &self.summary;
        let _ = writeln!(
            output,
            "{}  {}",
            theme.heading("UNPIN"),
            theme.strong(&summary.board_name)
        );
        let _ = writeln!(output, "{}", theme.dim("Pinterest duplicate review"));
        let _ = writeln!(output);
        let _ = writeln!(
            output,
            "{}  {} returned{}  {} analyzed  {} skipped",
            theme.label("PINS"),
            theme.strong(summary.pins_found),
            summary
                .pins_reported
                .map(|reported| format!(" / {reported} reported"))
                .unwrap_or_default(),
            theme.success(summary.analyzed),
            theme.warning(summary.skipped)
        );
        let _ = writeln!(
            output,
            "{}  {} exact group(s)  {} visual candidate(s)",
            theme.label("MATCHES"),
            theme.strong(summary.exact_groups),
            theme.accent(summary.visual_candidates)
        );

        if self.exact_groups.is_empty() && self.visual_candidates.is_empty() {
            let _ = writeln!(output, "\n{}", theme.dim("No duplicate image pins found."));
        }

        for (index, group) in self.exact_groups.iter().enumerate() {
            let _ = writeln!(
                output,
                "\n{}  {}",
                theme.section(format!("EXACT {:02}", index + 1)),
                theme.dim("byte-identical files")
            );
            render_items(&mut output, &group.items, &theme);
        }

        for (index, candidate) in self.visual_candidates.iter().enumerate() {
            let _ = writeln!(
                output,
                "\n{}  {}",
                theme.section(format!("VISUAL {:02}", index + 1)),
                theme.dim(format!(
                    "{}% similarity  •  hash distance {}/64",
                    candidate.similarity_percent, candidate.hash_distance
                ))
            );
            render_items(&mut output, &candidate.items, &theme);
        }

        if !self.skipped.is_empty() {
            let _ = writeln!(
                output,
                "\n{}  {}",
                theme.warning("SKIPPED"),
                theme.dim(format!("{} pin(s)", self.skipped.len()))
            );
            let mut reasons = BTreeMap::new();
            for skipped in &self.skipped {
                *reasons.entry(skipped.reason.as_str()).or_insert(0_usize) += 1;
            }
            let mut reasons = reasons.into_iter().collect::<Vec<_>>();
            reasons.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
            for (reason, count) in reasons {
                let _ = writeln!(
                    output,
                    "  {}  {}",
                    theme.warning(format!("{count:>3}×")),
                    reason
                );
            }

            if self.skipped.len() <= 12 {
                for skipped in &self.skipped {
                    if let (Some(id), Some(url)) = (&skipped.pin_id, &skipped.pin_url) {
                        let _ = writeln!(output, "       {}  {}", theme.dim(id), skipped.reason);
                        let _ = writeln!(
                            output,
                            "       {}  {}",
                            " ".repeat(id.len()),
                            theme.link(url)
                        );
                    }
                }
            } else {
                let _ = writeln!(
                    output,
                    "       {}",
                    theme.dim("Full skipped-pin details are in JSON and the visual report.")
                );
            }
        }

        if !self.warnings.is_empty() {
            let _ = writeln!(output, "\n{}", theme.warning("WARNINGS"));
            for warning in &self.warnings {
                let _ = writeln!(output, "  {} {warning}", theme.warning("!"));
            }
        }

        if let Some(path) = &self.visual_report {
            let _ = writeln!(
                output,
                "\n{}  {}",
                theme.label("VISUAL REPORT"),
                theme.link(path)
            );
        }

        output
    }

    pub fn render_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

fn render_items(output: &mut String, items: &[ReportItem], theme: &TextTheme) {
    for item in items {
        let status = format!("{:<8}", item.recommendation.label());
        let status = match item.recommendation {
            Recommendation::Keep => theme.success(status),
            Recommendation::Tie => theme.warning(status),
            Recommendation::DeleteCandidate => theme.danger(status),
        };
        let _ = writeln!(
            output,
            "  {}  {:>5} × {:<5}  {:>10}",
            status,
            item.width,
            item.height,
            human_bytes(item.byte_size)
        );
        let _ = writeln!(output, "            {}", theme.link(&item.pin_url));
    }
}

struct TextTheme {
    color: bool,
}

impl TextTheme {
    fn paint(&self, style: Style, value: impl ToString) -> String {
        let value = value.to_string();
        if self.color {
            style.force_styling(true).apply_to(value).to_string()
        } else {
            value
        }
    }

    fn heading(&self, value: impl ToString) -> String {
        self.paint(Style::new().cyan().bold(), value)
    }

    fn section(&self, value: impl ToString) -> String {
        self.paint(Style::new().magenta().bold(), value)
    }

    fn label(&self, value: impl ToString) -> String {
        self.paint(Style::new().blue().bold(), value)
    }

    fn strong(&self, value: impl ToString) -> String {
        self.paint(Style::new().bold(), value)
    }

    fn success(&self, value: impl ToString) -> String {
        self.paint(Style::new().green().bold(), value)
    }

    fn warning(&self, value: impl ToString) -> String {
        self.paint(Style::new().yellow().bold(), value)
    }

    fn danger(&self, value: impl ToString) -> String {
        self.paint(Style::new().red().bold(), value)
    }

    fn accent(&self, value: impl ToString) -> String {
        self.paint(Style::new().magenta().bold(), value)
    }

    fn dim(&self, value: impl ToString) -> String {
        self.paint(Style::new().dim(), value)
    }

    fn link(&self, value: impl ToString) -> String {
        self.paint(Style::new().cyan().underlined(), value)
    }
}

pub(crate) fn human_bytes(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    if bytes as f64 >= MIB {
        format!("{:.1} MiB", bytes as f64 / MIB)
    } else if bytes as f64 >= KIB {
        format!("{:.1} KiB", bytes as f64 / KIB)
    } else {
        format!("{bytes} B")
    }
}

pub(crate) fn rank_tuple(width: u32, height: u32, bytes: u64) -> (u64, u32, u64) {
    (
        u64::from(width) * u64::from(height),
        width.max(height),
        bytes,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_report_contains_links_and_recommendations() {
        let report = Report {
            summary: Summary {
                board_name: "Ideas".into(),
                pins_reported: Some(2),
                pins_found: 2,
                analyzed: 2,
                skipped: 0,
                exact_groups: 1,
                visual_candidates: 0,
            },
            exact_groups: vec![DuplicateGroup {
                items: vec![ReportItem {
                    pin_id: "123".into(),
                    pin_url: "https://www.pinterest.com/pin/123/".into(),
                    image_url: "https://i.pinimg.com/originals/example.jpg".into(),
                    width: 1200,
                    height: 800,
                    byte_size: 2048,
                    recommendation: Recommendation::Keep,
                }],
            }],
            visual_candidates: vec![],
            skipped: vec![],
            warnings: vec![],
            visual_report: Some("/tmp/unpin-example.html".into()),
        };

        let rendered = report.render_text();
        assert!(rendered.contains("EXACT 01"));
        assert!(rendered.contains("KEEP"));
        assert!(rendered.contains("https://www.pinterest.com/pin/123/"));
        assert!(rendered.contains("VISUAL REPORT"));
        assert!(rendered.contains("/tmp/unpin-example.html"));
        assert!(rendered.contains("2 returned / 2 reported"));
        assert!(!rendered.contains("\u{1b}["));

        let colored = report.render_text_with_color(true);
        assert!(colored.contains("\u{1b}["));
    }

    #[test]
    fn json_report_has_required_top_level_keys() {
        let report = Report {
            summary: Summary {
                board_name: "Empty".into(),
                pins_reported: None,
                pins_found: 0,
                analyzed: 0,
                skipped: 0,
                exact_groups: 0,
                visual_candidates: 0,
            },
            exact_groups: vec![],
            visual_candidates: vec![],
            skipped: vec![],
            warnings: vec![],
            visual_report: None,
        };
        let value: serde_json::Value =
            serde_json::from_str(&report.render_json().unwrap()).unwrap();
        for key in [
            "summary",
            "exact_groups",
            "visual_candidates",
            "skipped",
            "warnings",
        ] {
            assert!(value.get(key).is_some(), "{key}");
        }
    }

    #[test]
    fn text_report_groups_large_skipped_sets() {
        let skipped = (0..13)
            .map(|index| SkippedPin {
                pin_id: Some(index.to_string()),
                pin_url: Some(format!("https://www.pinterest.com/pin/{index}/")),
                reason: "video pin".into(),
            })
            .collect();
        let report = Report {
            summary: Summary {
                board_name: "Videos".into(),
                pins_reported: Some(13),
                pins_found: 13,
                analyzed: 0,
                skipped: 13,
                exact_groups: 0,
                visual_candidates: 0,
            },
            exact_groups: vec![],
            visual_candidates: vec![],
            skipped,
            warnings: vec![],
            visual_report: None,
        };

        let rendered = report.render_text();
        assert!(rendered.contains("13×"));
        assert!(rendered.contains("video pin"));
        assert!(rendered.contains("Full skipped-pin details"));
        assert!(!rendered.contains("https://www.pinterest.com/pin/0/"));
    }
}
