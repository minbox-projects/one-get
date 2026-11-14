//! Parsing for the machine-readable progress stream emitted by yt-dlp.
//!
//! The download passes `--progress-template` with a fixed prefix, three fields,
//! and `NA` for unknown values, so the service can persist byte counts without
//! interpreting human-readable output.

/// One progress observation from a `ONEGET_PROGRESS` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ProgressUpdate {
    /// Bytes downloaded so far.
    pub downloaded_bytes: u64,
    /// Reported total size, or `None` when yt-dlp reported `NA`.
    pub total_bytes: Option<u64>,
}

/// Parse one `ONEGET_PROGRESS <downloaded> <total|NA> <speed|NA>` line.
pub(crate) fn parse_progress_line(line: &str) -> Option<ProgressUpdate> {
    let mut fields = line.split_whitespace();
    if fields.next()? != "ONEGET_PROGRESS" {
        return None;
    }
    let downloaded_bytes = fields.next()?.parse::<u64>().ok()?;
    let total_bytes = match fields.next()? {
        "NA" => None,
        value => Some(value.parse::<u64>().ok()?),
    };
    // The third field is the speed. It is part of the fixed template but the
    // durable task record keeps only byte counts.
    let _speed = fields.next()?;
    Some(ProgressUpdate {
        downloaded_bytes,
        total_bytes,
    })
}

/// Whether a line marks the ffmpeg merge stage.
pub(crate) fn is_merge_line(line: &str) -> bool {
    line.trim_start().starts_with("[Merger]")
}
