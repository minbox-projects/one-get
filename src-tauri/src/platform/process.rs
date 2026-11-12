//! Guarded child processes that own a whole process tree.
//!
//! On Unix the child becomes the leader of a new process group; on Windows it
//! is assigned to a Job Object. Killing the guard terminates the whole tree,
//! and a `Drop` fallback ensures a running tree cannot outlive it.

use std::process::ExitStatus;
use std::time::Duration;

#[cfg(windows)]
use process_wrap::tokio::JobObject;
#[cfg(unix)]
use process_wrap::tokio::ProcessGroup;
use process_wrap::tokio::{ChildWrapper, CommandWrap, KillOnDrop};
use tokio::process::Command;

use super::PlatformError;

/// A spawned child process whose group or job is owned by this guard.
pub struct ChildGuard {
    child: Box<dyn ChildWrapper>,
}

impl ChildGuard {
    /// Spawn `command` as the leader of a process group on Unix or inside a
    /// Job Object on Windows, with kill-on-drop enabled.
    pub fn spawn(command: Command) -> Result<Self, PlatformError> {
        let mut wrapped = CommandWrap::from(command);
        #[cfg(unix)]
        wrapped.wrap(ProcessGroup::leader());
        #[cfg(windows)]
        wrapped.wrap(JobObject);
        wrapped.wrap(KillOnDrop);

        let child = wrapped
            .spawn()
            .map_err(|error| PlatformError::io_error("spawn child process", error))?;
        Ok(Self { child })
    }

    /// The top-level child process identifier.
    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    /// Terminate the entire process tree and wait for the top-level child.
    ///
    /// Returns success when the child has already exited, making repeated calls
    /// idempotent.
    pub async fn kill_tree(&mut self) -> Result<(), PlatformError> {
        match self.child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => {}
            Err(error) => {
                return Err(PlatformError::io_error(
                    "inspect child process before killing",
                    error,
                ));
            }
        }

        self.child
            .start_kill()
            .map_err(|error| PlatformError::io_error("kill child process tree", error))?;
        self.child
            .wait()
            .await
            .map_err(|error| PlatformError::io_error("reap child process tree", error))?;
        Ok(())
    }

    /// Wait for the child to exit within `duration`.
    ///
    /// On timeout the whole tree is terminated and a `timeout` error is
    /// returned.
    pub async fn wait_timeout(&mut self, duration: Duration) -> Result<ExitStatus, PlatformError> {
        match tokio::time::timeout(duration, self.child.wait()).await {
            Ok(Ok(status)) => Ok(status),
            Ok(Err(error)) => Err(PlatformError::io_error("wait for child process", error)),
            Err(_) => {
                let _ = self.kill_tree().await;
                Err(PlatformError::new(
                    "timeout",
                    format!("child process did not exit within {duration:?}"),
                ))
            }
        }
    }
}

impl Drop for ChildGuard {
    fn drop(&mut self) {
        // Killing is asynchronous, but a still-running tree must not outlive
        // the guard, so signal the group or job synchronously here.
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.start_kill();
        }
    }
}
