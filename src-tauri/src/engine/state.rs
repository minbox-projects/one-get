//! Task and segment state machines shared by the engine and the store.

/// Lifecycle of a download task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskState {
    /// Waiting for a scheduler slot.
    Queued,
    /// Resolving the remote file and planning segments.
    Connecting,
    /// Workers are writing bytes into the part file.
    Downloading,
    /// Progress is persisted and all workers have stopped.
    Paused,
    /// Bytes are complete and the part file is being hashed and renamed.
    Finalizing,
    /// The final file exists and its hash is stored.
    Completed,
    /// The task stopped with an error and preserves its part file for retry.
    Failed,
    /// The task was cancelled and its own part file was removed.
    Cancelled,
}

impl TaskState {
    /// Stable lowercase representation stored in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Connecting => "connecting",
            Self::Downloading => "downloading",
            Self::Paused => "paused",
            Self::Finalizing => "finalizing",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    /// Parse a stored state value.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "queued" => Some(Self::Queued),
            "connecting" => Some(Self::Connecting),
            "downloading" => Some(Self::Downloading),
            "paused" => Some(Self::Paused),
            "finalizing" => Some(Self::Finalizing),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    /// States that occupy a scheduler concurrency slot.
    pub fn is_active(self) -> bool {
        matches!(
            self,
            Self::Connecting | Self::Downloading | Self::Finalizing
        )
    }

    /// Whether the state machine permits `next` after `self`.
    pub fn can_transition_to(self, next: Self) -> bool {
        use TaskState::{
            Cancelled, Completed, Connecting, Downloading, Failed, Finalizing, Paused, Queued,
        };

        matches!(
            (self, next),
            (Queued, Connecting | Paused | Cancelled)
                | (
                    Connecting,
                    Queued | Downloading | Paused | Failed | Cancelled
                )
                | (
                    Downloading,
                    Queued | Paused | Finalizing | Failed | Cancelled
                )
                | (Paused, Queued | Failed | Cancelled)
                | (Finalizing, Completed | Failed | Cancelled)
                | (Failed, Queued | Cancelled)
                | (Cancelled, Cancelled)
        )
    }
}

/// Lifecycle of one byte range owned by a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SegmentState {
    /// The range has not been requested yet.
    Pending,
    /// A worker currently owns the range.
    Active,
    /// The range is fully written.
    Done,
    /// The range stopped with an error and can be retried.
    Failed,
}

impl SegmentState {
    /// Stable lowercase representation stored in the database.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Done => "done",
            Self::Failed => "failed",
        }
    }

    /// Parse a stored state value or a derived `Debug` variant name.
    pub fn parse(raw: &str) -> Option<Self> {
        if raw.eq_ignore_ascii_case("pending") {
            Some(Self::Pending)
        } else if raw.eq_ignore_ascii_case("active") {
            Some(Self::Active)
        } else if raw.eq_ignore_ascii_case("done") {
            Some(Self::Done)
        } else if raw.eq_ignore_ascii_case("failed") {
            Some(Self::Failed)
        } else {
            None
        }
    }

    /// Whether the state machine permits `next` after `self`.
    pub fn can_transition_to(self, next: Self) -> bool {
        use SegmentState::{Active, Done, Failed, Pending};

        matches!(
            (self, next),
            (Pending, Active | Failed) | (Active, Done | Failed | Pending) | (Failed, Pending)
        )
    }
}
