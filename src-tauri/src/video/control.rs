//! Shared cancellation signal for one running video download.
//!
//! The service actor requests a pause or a cancel and the guarded process runner
//! watches the same handle, so the whole process tree is killed as soon as the
//! control arrives instead of after the next progress line.

use std::sync::{Arc, Mutex};

use tokio::sync::Notify;

/// The control that ended a running download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlAction {
    /// Pause the task and preserve its temporary directory.
    Pause,
    /// Cancel the task and remove its task-owned files.
    Cancel,
}

/// A cloneable handle that carries at most one control request.
#[derive(Clone)]
pub struct DownloadControl {
    inner: Arc<Inner>,
}

struct Inner {
    action: Mutex<Option<ControlAction>>,
    notify: Notify,
}

impl DownloadControl {
    /// Create an idle control handle.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                action: Mutex::new(None),
                notify: Notify::new(),
            }),
        }
    }

    /// Request a control action. The first request wins; later ones are ignored.
    pub fn request(&self, action: ControlAction) {
        if let Ok(mut current) = self.inner.action.lock() {
            if current.is_none() {
                *current = Some(action);
            }
        }
        self.inner.notify.notify_waiters();
    }

    /// The requested action, when one has arrived.
    pub fn reason(&self) -> Option<ControlAction> {
        self.inner.action.lock().ok().and_then(|current| *current)
    }

    /// Resolve as soon as a control request may have arrived.
    pub async fn notified(&self) {
        self.inner.notify.notified().await;
    }
}

impl Default for DownloadControl {
    fn default() -> Self {
        Self::new()
    }
}
