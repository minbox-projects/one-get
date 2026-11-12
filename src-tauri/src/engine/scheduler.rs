//! FIFO task scheduling with a runtime-adjustable concurrency limit.
//!
//! The scheduler claims queued tasks in creation order, runs them concurrently
//! with borrowed futures, and keeps the number of connecting, downloading, and
//! finalizing tasks at or below the configured limit. Pause, resume, cancel,
//! and retry operate on the shared task controls so a caller can stop or revive
//! a task while the engine loop is running.

use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::stream::FuturesUnordered;
use futures_util::StreamExt;
use tokio::sync::Notify;

use crate::store::types::Task;
use crate::store::Database;

use super::error::EngineError;
use super::state::TaskState;
use super::worker::{
    cancel_task, download_task, persist_pause, DownloadSpec, TaskControl, TaskRunOutcome,
};

/// How long the scheduler waits for newly added work before it stops.
const IDLE_GRACE: Duration = Duration::from_millis(1500);

/// How often a control operation re-reads task state while waiting for workers.
const CONTROL_POLL: Duration = Duration::from_millis(5);

type TaskFuture<'a> = Pin<Box<dyn Future<Output = Result<(), EngineError>> + 'a>>;

/// Construction options for an [`Engine`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EngineOptions {
    /// Maximum number of tasks in connecting, downloading, or finalizing state.
    pub max_concurrent: usize,
}

/// A durable, queue-driven download engine.
pub struct Engine {
    store: Database,
    dest_dir: PathBuf,
    max_concurrent: AtomicUsize,
    controls: Mutex<HashMap<String, Arc<TaskControl>>>,
    notify: Notify,
}

impl Engine {
    /// Create an engine over an open task store and destination directory.
    pub fn new(store: Database, dest_dir: PathBuf, options: EngineOptions) -> Self {
        Self {
            store,
            dest_dir,
            max_concurrent: AtomicUsize::new(options.max_concurrent),
            controls: Mutex::new(HashMap::new()),
            notify: Notify::new(),
        }
    }

    /// The durable store owned by this engine.
    pub fn store(&self) -> &Database {
        &self.store
    }

    /// Create a queued task and persist its connection and hash settings.
    pub fn add_task(&self, spec: DownloadSpec) -> Result<String, EngineError> {
        let id = self.store.create_task(&spec.url, &spec.filename)?;
        self.store.record_connections(&id, spec.connections)?;
        if let Some(expected) = spec.expected_sha256.as_deref() {
            self.store.record_expected_sha256(&id, expected)?;
        }
        self.notify.notify_one();
        Ok(id)
    }

    /// Change the concurrency limit without restarting the engine.
    pub fn set_max_concurrent(&self, max: usize) -> Result<(), EngineError> {
        self.max_concurrent.store(max, Ordering::SeqCst);
        self.notify.notify_one();
        Ok(())
    }

    /// Run until no queued or active tasks remain.
    ///
    /// Paused tasks do not consume a slot and do not keep the loop alive. A
    /// short idle grace lets a caller add work immediately after the queue
    /// drains, so pause-then-add and retry flows do not need a new run call.
    pub async fn run(&self) -> Result<(), EngineError> {
        let mut active: FuturesUnordered<TaskFuture<'_>> = FuturesUnordered::new();

        loop {
            while let Some(task) = self.claim_next()? {
                let control = self.control_for(&task.id);
                active.push(Box::pin(self.run_task(task.id, control)));
            }

            if active.is_empty() {
                if self.store.queued_task_count()? == 0 && self.store.active_task_count()? == 0 {
                    match tokio::time::timeout(IDLE_GRACE, self.notify.notified()).await {
                        Ok(()) => continue,
                        Err(_) => {
                            if self.store.queued_task_count()? == 0
                                && self.store.active_task_count()? == 0
                            {
                                return Ok(());
                            }
                            continue;
                        }
                    }
                }
                tokio::time::sleep(CONTROL_POLL).await;
                continue;
            }

            tokio::select! {
                _ = active.next() => {}
                _ = self.notify.notified() => {}
            }
        }
    }

    /// Pause a queued or running task and persist its progress.
    pub async fn pause(&self, id: &str) -> Result<(), EngineError> {
        let task = load_task(&self.store, id)?;
        match task.state {
            TaskState::Queued => {
                self.store
                    .transition_task(id, TaskState::Queued, TaskState::Paused)
            }
            TaskState::Paused => Ok(()),
            TaskState::Connecting | TaskState::Downloading => match self.existing_control(id) {
                Some(control) => {
                    control.request_pause();
                    self.notify.notify_one();
                    self.wait_for(id, TaskState::Paused).await
                }
                None => persist_pause(&self.store, id),
            },
            TaskState::Finalizing
            | TaskState::Completed
            | TaskState::Failed
            | TaskState::Cancelled => Err(EngineError::invalid_state(
                "the task cannot be paused in its current state",
            )),
        }
    }

    /// Requeue a paused task; ignore states that are already active.
    pub async fn resume(&self, id: &str) -> Result<(), EngineError> {
        let task = load_task(&self.store, id)?;
        match task.state {
            TaskState::Paused => {
                self.store
                    .transition_task(id, TaskState::Paused, TaskState::Queued)?;
                self.notify.notify_one();
                Ok(())
            }
            TaskState::Queued
            | TaskState::Connecting
            | TaskState::Downloading
            | TaskState::Finalizing => Ok(()),
            TaskState::Completed | TaskState::Failed | TaskState::Cancelled => Err(
                EngineError::invalid_state("the task cannot be resumed in its current state"),
            ),
        }
    }

    /// Cancel a task and remove only its own part file.
    pub async fn cancel(&self, id: &str) -> Result<(), EngineError> {
        let task = load_task(&self.store, id)?;
        match task.state {
            TaskState::Queued | TaskState::Paused | TaskState::Failed | TaskState::Finalizing => {
                cancel_task(&self.store, id, &self.dest_dir)
            }
            TaskState::Connecting | TaskState::Downloading => match self.existing_control(id) {
                Some(control) => {
                    control.request_cancel();
                    self.notify.notify_one();
                    self.wait_for(id, TaskState::Cancelled).await
                }
                None => cancel_task(&self.store, id, &self.dest_dir),
            },
            TaskState::Cancelled => Ok(()),
            TaskState::Completed => Err(EngineError::invalid_state(
                "a completed task cannot be cancelled",
            )),
        }
    }

    /// Requeue a failed task while preserving its part file.
    pub async fn retry(&self, id: &str) -> Result<(), EngineError> {
        let task = load_task(&self.store, id)?;
        if task.state != TaskState::Failed {
            return Err(EngineError::invalid_state(
                "only a failed task can be retried",
            ));
        }
        self.store
            .transition_task(id, TaskState::Failed, TaskState::Queued)?;
        self.notify.notify_one();
        Ok(())
    }

    fn claim_next(&self) -> Result<Option<Task>, EngineError> {
        let limit = self.max_concurrent.load(Ordering::SeqCst);
        let active_count = self.store.active_task_count()? as usize;
        if active_count >= limit {
            return Ok(None);
        }
        self.store.claim_oldest_queued_task()
    }

    fn control_for(&self, id: &str) -> Arc<TaskControl> {
        let mut controls = self
            .controls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(control) = controls.get(id) {
            return Arc::clone(control);
        }
        let control = Arc::new(TaskControl::new());
        controls.insert(id.to_string(), Arc::clone(&control));
        control
    }

    fn existing_control(&self, id: &str) -> Option<Arc<TaskControl>> {
        self.controls
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(id)
            .cloned()
    }

    async fn wait_for(&self, id: &str, expected: TaskState) -> Result<(), EngineError> {
        loop {
            let task = load_task(&self.store, id)?;
            if task.state == expected {
                return Ok(());
            }
            if matches!(
                task.state,
                TaskState::Completed | TaskState::Failed | TaskState::Cancelled
            ) {
                return Err(EngineError::invalid_state(
                    "the task reached an incompatible state before the control request completed",
                ));
            }
            tokio::time::sleep(CONTROL_POLL).await;
        }
    }

    async fn run_task(
        &self,
        task_id: String,
        control: Arc<TaskControl>,
    ) -> Result<(), EngineError> {
        let result = match self.store.task(&task_id)? {
            Some(task) => {
                let spec = spec_from_task(&task);
                download_task(&self.store, &task_id, &self.dest_dir, &spec, &control).await
            }
            None => Ok(TaskRunOutcome::Cancelled),
        };

        {
            let mut controls = self
                .controls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            controls.remove(&task_id);
        }

        match result {
            Ok(_) => Ok(()),
            Err(error) => {
                if let Ok(Some(task)) = self.store.task(&task_id) {
                    if task.state.can_transition_to(TaskState::Failed) {
                        let _ = self
                            .store
                            .transition_task(&task_id, task.state, TaskState::Failed);
                    }
                    let _ =
                        self.store
                            .update_task_error(&task_id, error.code(), &error.to_string());
                }
                if error.code() == "DATABASE_ERROR" {
                    return Err(error);
                }
                Ok(())
            }
        }
    }
}

fn spec_from_task(task: &Task) -> DownloadSpec {
    DownloadSpec {
        url: task.url.clone(),
        filename: task.filename.clone(),
        connections: task.connections.max(1),
        allow_html: false,
        expected_sha256: task.expected_sha256.clone(),
        extra_headers: Vec::new(),
    }
}

fn load_task(store: &Database, id: &str) -> Result<Task, EngineError> {
    store
        .task(id)?
        .ok_or_else(|| EngineError::new("TASK_NOT_FOUND", "the requested task does not exist"))
}
