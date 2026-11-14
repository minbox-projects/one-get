//! Public records returned by the durable task store.

use crate::engine::state::{SegmentState, TaskState};

/// Which pipeline owns and runs a persisted task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskKind {
    /// The byte-range HTTP download engine.
    Http,
    /// The external-tool video pipeline.
    Video,
}

impl TaskKind {
    /// Stable lowercase representation stored in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Video => "video",
        }
    }

    /// Parse a stored task kind.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "http" => Some(Self::Http),
            "video" => Some(Self::Video),
            _ => None,
        }
    }
}

/// Input used to persist a new video task.
#[derive(Debug, Clone, PartialEq)]
pub struct VideoTaskInput {
    /// Minimized source URL; the full URL stays in memory.
    pub url: String,
    /// Sanitized destination filename.
    pub filename: String,
    /// Destination directory, or `None` for the configured default.
    pub destination: Option<String>,
    /// Selected yt-dlp format selector.
    pub format_selector: String,
    /// Optional Netscape cookies-file path passed to yt-dlp.
    pub cookies_file: Option<String>,
    /// Optional credential-free HTTP proxy URL passed to yt-dlp.
    pub proxy_url: Option<String>,
    /// Parsed probe metadata stored for the task details.
    pub metadata: serde_json::Value,
    /// Whether the full source URL survived persistence.
    pub full_url_persisted: bool,
}

/// Observable configuration and structure of the open database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaMetadata {
    /// Applied schema version.
    pub version: i64,
    /// Active SQLite journal mode.
    pub journal_mode: String,
    /// Whether foreign-key enforcement is enabled for this connection.
    pub foreign_keys_enabled: bool,
    /// Configured busy timeout in milliseconds.
    pub busy_timeout_ms: i64,
    /// Whether the queue and segment indexes exist.
    pub has_required_indexes: bool,
    /// Whether the tables carry the required keys, state checks, and foreign key.
    pub has_required_constraints: bool,
}

/// A persisted download task.
#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    /// Stable task identifier.
    pub id: String,
    /// Pipeline that owns and runs this task.
    pub kind: TaskKind,
    /// Structured video record for a `video` task.
    pub video: Option<serde_json::Value>,
    /// Original remote URL.
    pub url: String,
    /// Final URL after redirects, stored without userinfo, query, or fragment.
    pub final_url: Option<String>,
    /// Sanitized destination filename.
    pub filename: String,
    /// Persisted destination directory, or `None` for the engine default.
    pub destination: Option<String>,
    /// Part file recorded for recovery.
    pub part_path: Option<String>,
    /// Current lifecycle state.
    pub state: TaskState,
    /// Bytes confirmed written into the part file.
    pub downloaded_bytes: u64,
    /// Known total size, when the remote reported one.
    pub total_bytes: Option<u64>,
    /// Expected SHA-256 supplied by the caller.
    pub expected_sha256: Option<String>,
    /// SHA-256 computed during finalization.
    pub actual_sha256: Option<String>,
    /// Remote entity tag discovered during probing.
    pub etag: Option<String>,
    /// Remote last-modified value discovered during probing.
    pub last_modified: Option<String>,
    /// Stable failure code.
    pub error_code: Option<String>,
    /// User-safe failure message.
    pub error_message: Option<String>,
    /// Creation time in epoch milliseconds.
    pub created_at_ms: i64,
    /// Last modification time in epoch milliseconds.
    pub updated_at_ms: i64,
    /// Completion time in epoch milliseconds, once the task completed.
    pub completed_at_ms: Option<i64>,
    /// Requested parallel connection count.
    pub connections: u32,
}

/// A persisted byte range owned by a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    /// Owning task identifier.
    pub task_id: String,
    /// Zero-based segment index within the task.
    pub index: u32,
    /// Inclusive first byte offset.
    pub start: u64,
    /// Exclusive end offset.
    pub end: u64,
    /// Bytes confirmed written for this range.
    pub downloaded_bytes: u64,
    /// Current lifecycle state.
    pub state: SegmentState,
}

/// Descriptor used to replace a task's segment plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentInput {
    /// Zero-based segment index.
    pub index: u32,
    /// Inclusive first byte offset.
    pub start: u64,
    /// Exclusive end offset.
    pub end: u64,
    /// Bytes already written for this range.
    pub downloaded: u64,
    /// Initial lifecycle state.
    pub state: SegmentState,
}

/// Descriptor used to persist one segment's progress snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SegmentProgressInput {
    /// Zero-based segment index.
    pub segment_id: u32,
    /// Bytes confirmed written for this range.
    pub downloaded_bytes: u64,
}
