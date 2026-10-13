//! Versioned scan interchange and safe, atomic report output.
use std::io::Write;
use std::path::Path;

use serde::Serialize;
use thiserror::Error;

use crate::report::Report;

const SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Error)]
pub enum SavedReportError {
    #[error("report file could not be read or written")]
    Io(#[from] std::io::Error),
    #[error("invalid saved report")]
    Json(#[from] serde_json::Error),
    #[error("unsupported saved report schema; expected version 1 or 2")]
    Version,
    #[error("saved report contains a link that is not an HTTP or HTTPS URL")]
    UnsafeLink,
}

pub fn serialize(report: &Report) -> Result<String, serde_json::Error> {
    #[derive(Serialize)]
    struct Saved<'a> {
        schema_version: u32,
        #[serde(flatten)]
        report: &'a Report,
    }
    serde_json::to_string_pretty(&Saved {
        schema_version: SCHEMA_VERSION,
        report,
    })
}

pub fn read(path: &Path) -> Result<Report, SavedReportError> {
    let value: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    if !matches!(
        value.get("schema_version").and_then(|v| v.as_u64()),
        Some(1 | 2)
    ) {
        return Err(SavedReportError::Version);
    }
    let mut report: Report = serde_json::from_value(value)?;
    report.normalize_preview_recommendations();
    // Escaping HTML alone does not make an href safe. Saved reports may have
    // been edited or supplied by someone else.
    let items = report.exact_groups.iter().flat_map(|g| &g.items).chain(
        report
            .visual_candidates
            .iter()
            .flat_map(|c| c.items.iter().chain(c.copies.iter().flatten())),
    );
    for item in items {
        check_link(&item.pin_url)?;
        check_link(&item.image_url)?;
    }
    for board in &report.summary.boards {
        if !board.url.is_empty() {
            check_link(&board.url)?;
        }
    }
    for pin in &report.skipped {
        if let Some(url) = &pin.pin_url {
            check_link(url)?;
        }
    }
    Ok(report)
}

fn check_link(value: &str) -> Result<(), SavedReportError> {
    match url::Url::parse(value) {
        Ok(url) if matches!(url.scheme(), "http" | "https") && url.host_str().is_some() => Ok(()),
        _ => Err(SavedReportError::UnsafeLink),
    }
}

/// Publish only a complete file, and never overwrite an existing report.
pub fn write(path: &Path, bytes: &[u8]) -> Result<(), SavedReportError> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    temporary.persist_noclobber(path).map_err(|e| e.error)?;
    Ok(())
}
