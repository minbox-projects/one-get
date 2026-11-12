//! Deterministic segment planning for ranged and open-ended downloads.

use super::error::EngineError;

/// Smallest byte range the planner creates unless the whole file is smaller or
/// the segment is the final remainder.
pub const MIN_SEGMENT_BYTES: u64 = 1024 * 1024;

/// Largest number of parallel connections the planner accepts.
pub const MAX_CONNECTIONS: u32 = 64;

/// One half-open byte range `[start, end)` owned by a worker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedSegment {
    /// Zero-based position in the plan.
    pub index: u32,
    /// Inclusive first byte offset.
    pub start: u64,
    /// Exclusive end offset.
    pub end: u64,
}

/// A complete download plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SegmentPlan {
    /// Known length with range support: gap-free half-open coverage.
    Ranged(Vec<PlannedSegment>),
    /// Unknown length or no range support: one connection from `start`.
    OpenEnded {
        /// First byte offset of the single connection.
        start: u64,
    },
}

/// Plan 1 to 64 non-overlapping, gap-free segments.
///
/// A known total with range support is split into
/// `min(connections, max(1, total / MIN_SEGMENT_BYTES))` equal segments. A
/// total of zero produces an empty ranged plan. An unknown total or a server
/// without range support produces one open-ended segment.
pub fn plan_segments(
    total: Option<u64>,
    range_supported: bool,
    connections: u32,
) -> Result<SegmentPlan, EngineError> {
    if connections == 0 || connections > MAX_CONNECTIONS {
        return Err(EngineError::new(
            "INVALID_CONNECTIONS",
            format!("connections must be between 1 and {MAX_CONNECTIONS}"),
        ));
    }

    let Some(total) = total.filter(|_| range_supported) else {
        return Ok(SegmentPlan::OpenEnded { start: 0 });
    };

    if total == 0 {
        return Ok(SegmentPlan::Ranged(Vec::new()));
    }

    let by_minimum = std::cmp::max(1, total / MIN_SEGMENT_BYTES);
    let count = std::cmp::min(u64::from(connections), by_minimum);
    let base = total / count;
    let remainder = total % count;

    let mut segments = Vec::with_capacity(count as usize);
    let mut start = 0u64;
    for index in 0..count {
        let length = base + u64::from(index < remainder);
        let end = start + length;
        segments.push(PlannedSegment {
            index: index as u32,
            start,
            end,
        });
        start = end;
    }

    Ok(SegmentPlan::Ranged(segments))
}
