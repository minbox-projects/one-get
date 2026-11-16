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
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use futures_util::stream::FuturesUnordered;
use futures_util::StreamExt;
use reqwest::Client;
use tokio::sync::Notify;

use crate::commands::downloads::DownloadSummary;
use crate::logging::{LogLevel, Logger};
use crate::platform::opener::open_target;
use crate::platform::secrets::SecretValue;
use crate::platform::PlatformError;
use crate::platform::{current_target, sanitize_filename, CapDir};
use crate::store::hf::{HfMetadata, HfMetadataStore};
use crate::store::types::{Task, TaskKind};
use crate::store::Database;

use super::cookies::{CookieJar, CookieSource};
use super::error::EngineError;
use super::hf;
use super::http::{self, ProxyConfig};
use super::rate_limit::RateLimiter;
use super::state::TaskState;
use super::worker::{
    cancel_task, download_task_with_network, finalize_task, find_completed_final, load_task,
    mark_failed, part_exists_for_task, part_missing, persist_pause, remote_metadata,
    task_part_name, DownloadSpec, NetworkContext, TaskControl, TaskRunOutcome,
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

/// Transport settings applied to later engine requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkOptions {
    /// Fallback User-Agent for requests without a per-task value.
    pub user_agent: Option<String>,
    /// Timeout for establishing a connection.
    pub connect_timeout: Duration,
    /// Timeout for reading from an established connection.
    pub read_timeout: Duration,
    /// Attempts one request may make before it gives up.
    pub retry_attempts: usize,
}

impl Default for NetworkOptions {
    fn default() -> Self {
        Self {
            user_agent: None,
            connect_timeout: Duration::from_secs(15),
            read_timeout: Duration::from_secs(30),
            retry_attempts: 5,
        }
    }
}

/// Hugging Face token, connection cap, and metadata store applied to later
/// task starts.
#[derive(Clone)]
pub struct HuggingFaceOptions {
    /// Access token sent only to resolver-host requests.
    pub token: Option<SecretValue>,
    /// Maximum connections one Hugging Face task may use.
    pub connection_cap: u32,
    /// Optional durable metadata store.
    pub metadata: Option<Arc<HfMetadataStore>>,
}

impl Default for HuggingFaceOptions {
    fn default() -> Self {
        Self {
            token: None,
            connection_cap: 8,
            metadata: None,
        }
    }
}

/// Summary of the store and part-file recovery performed at startup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecoveryReport {
    /// Connecting or downloading tasks requeued for a later resume.
    pub requeued: usize,
    /// Finalizing tasks completed through the idempotent finalize path.
    pub finalized: usize,
    /// Cancelled tasks whose leftover part files were removed.
    pub cancelled: usize,
    /// Tasks failed during recovery because their part or hash was invalid.
    pub failed: usize,
    /// Paused tasks left untouched for a later explicit resume.
    pub paused: usize,
}

/// A durable, queue-driven download engine.
pub struct Engine {
    store: Database,
    dest_dir: PathBuf,
    max_concurrent: AtomicUsize,
    controls: Mutex<HashMap<String, Arc<TaskControl>>>,
    notify: Notify,
    /// Full in-memory specs, including query-bearing URLs and header values.
    specs: Mutex<HashMap<String, DownloadSpec>>,
    /// Memory-only cookie source per task; captured values are never persisted.
    task_cookie_sources: Mutex<HashMap<String, CookieSource>>,
    proxy: RwLock<Option<ProxyConfig>>,
    network_options: RwLock<NetworkOptions>,
    huggingface: RwLock<HuggingFaceOptions>,
    client: Mutex<Option<Client>>,
    cookie_file: Mutex<Option<PathBuf>>,
    global_rate: Mutex<Option<Arc<RateLimiter>>>,
    task_rates: Mutex<HashMap<String, Arc<RateLimiter>>>,
    logger: Mutex<Option<Arc<Logger>>>,
    startup_done: AtomicBool,
    stop: Mutex<Option<Arc<AtomicBool>>>,
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
            specs: Mutex::new(HashMap::new()),
            task_cookie_sources: Mutex::new(HashMap::new()),
            proxy: RwLock::new(None),
            network_options: RwLock::new(NetworkOptions::default()),
            huggingface: RwLock::new(HuggingFaceOptions::default()),
            client: Mutex::new(None),
            cookie_file: Mutex::new(None),
            global_rate: Mutex::new(None),
            task_rates: Mutex::new(HashMap::new()),
            logger: Mutex::new(None),
            startup_done: AtomicBool::new(false),
            stop: Mutex::new(None),
        }
    }

    /// Share the service shutdown flag so a stop is not missed by a claim that
    /// races the request loop.
    pub(crate) fn set_stop_flag(&self, flag: Arc<AtomicBool>) {
        *self
            .stop
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(flag);
    }

    fn stop_requested(&self) -> bool {
        self.stop
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::SeqCst))
    }

    /// The durable store owned by this engine.
    pub fn store(&self) -> &Database {
        &self.store
    }

    /// Route every engine request through an optional HTTP proxy.
    ///
    /// The proxy URL is validated immediately. Embedded userinfo is refused so
    /// credentials can only arrive through the dedicated fields, which stay in
    /// memory and are never persisted or logged.
    pub fn set_proxy(&self, proxy: Option<ProxyConfig>) -> Result<(), EngineError> {
        let validated = match proxy {
            Some(proxy) => Some(ProxyConfig {
                url: http::validate_proxy_url(&proxy.url)?,
                username: proxy.username,
                password: proxy.password,
            }),
            None => None,
        };

        *self
            .proxy
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = validated;
        *self
            .client
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
        Ok(())
    }

    /// Replace the User-Agent fallback, timeouts, and retry budget used by
    /// later engine requests.
    ///
    /// The cached client is discarded so a new request rebuilds it with these
    /// values. A request already in flight keeps the client and options it
    /// started with.
    pub fn set_network_options(&self, options: NetworkOptions) {
        let mut cached = self
            .client
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        *self
            .network_options
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = options;
        *cached = None;
    }

    /// Replace the Hugging Face token, connection cap, and metadata store used
    /// by task starts after this call.
    ///
    /// A task already running keeps the effective connection count captured
    /// when it started.
    pub fn set_huggingface_options(&self, options: HuggingFaceOptions) -> Result<(), EngineError> {
        if !(1..=64).contains(&options.connection_cap) {
            return Err(EngineError::new(
                "INVALID_CONNECTIONS",
                "the Hugging Face connection cap must be between 1 and 64",
            ));
        }
        *self
            .huggingface
            .write()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = options;
        self.notify.notify_one();
        Ok(())
    }

    /// Load a Netscape cookie file at each task start or resume.
    pub fn set_cookie_file(&self, path: Option<PathBuf>) {
        *self
            .cookie_file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = path;
    }

    /// Pace every task's downloads at a shared bytes-per-second limit.
    ///
    /// `None` or zero removes the limit without restarting the engine. A
    /// running task keeps the limiter captured when its network context started;
    /// changes apply to task starts (including a later resume) after this call.
    pub fn set_global_rate_limit(&self, bytes_per_second: Option<u64>) {
        *self
            .global_rate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = active_limiter(bytes_per_second);
        self.notify.notify_one();
    }

    /// Pace one task's workers at a shared bytes-per-second limit.
    ///
    /// `None` or zero removes the limit without restarting the engine. A
    /// running task keeps the limiter captured when its network context started;
    /// changes apply to task starts (including a later resume) after this call.
    pub fn set_task_rate_limit(&self, task_id: &str, bytes_per_second: Option<u64>) {
        let mut limits = self
            .task_rates
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match active_limiter(bytes_per_second) {
            Some(limiter) => {
                limits.insert(task_id.to_string(), limiter);
            }
            None => {
                limits.remove(task_id);
            }
        }
        drop(limits);
        self.notify.notify_one();
    }

    /// Attach the structured logger that receives task lifecycle events.
    pub fn set_logger(&self, logger: Arc<Logger>) {
        *self
            .logger
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(logger);
    }

    /// Create a queued task and persist its connection and hash settings.
    ///
    /// The filename is sanitized with the current platform's naming rules
    /// before it reaches the store or the in-memory spec, so a caller cannot
    /// introduce a path component that later escapes the destination root.
    pub fn add_task(&self, spec: DownloadSpec) -> Result<String, EngineError> {
        http::validate_extra_headers(&spec.extra_headers)?;
        let mut spec = spec;
        spec.url = http::validate_url(&spec.url)?;
        spec.filename = sanitize_filename(&spec.filename, current_target());
        let destination = self.resolve_destination(spec.destination.as_deref())?;
        let stored_destination = destination
            .to_str()
            .ok_or_else(|| invalid_destination("the destination path is not valid UTF-8"))?;

        let stored_url = http::storage_url(&spec.url);
        let default_destination = std::fs::canonicalize(&self.dest_dir)
            .ok()
            .and_then(|path| path.to_str().map(str::to_string))
            .or_else(|| self.dest_dir.to_str().map(str::to_string))
            .unwrap_or_default();
        let (id, _part_name) = self.store.create_task_with_part(
            &stored_url,
            &spec.filename,
            Some(stored_destination),
            &default_destination,
        )?;
        self.store.record_connections(&id, spec.connections)?;
        if let Some(expected) = spec.expected_sha256.as_deref() {
            self.store.record_expected_sha256(&id, expected)?;
        }
        let header_names: Vec<String> = spec
            .extra_headers
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        self.store.record_header_names(&id, &header_names)?;

        spec.destination = Some(destination);
        self.specs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id.clone(), spec);
        self.notify.notify_one();
        Ok(id)
    }

    /// Create a queued task with exactly one memory-only cookie source.
    ///
    /// The source is retained for this process only; a captured browser cookie
    /// list is never written to the store and so cannot survive a restart. A
    /// file source must be absolute, because a relative path would otherwise be
    /// resolved against the process working directory.
    pub fn add_task_with_cookies(
        &self,
        spec: DownloadSpec,
        source: CookieSource,
    ) -> Result<String, EngineError> {
        if let CookieSource::File(path) = &source {
            if !path.is_absolute() {
                return Err(EngineError::new(
                    "INVALID_COOKIES_FILE",
                    "the cookies file path must be absolute",
                ));
            }
        }
        let id = self.add_task(spec)?;
        self.task_cookie_sources
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(id.clone(), source);
        Ok(id)
    }

    /// Every task as a lightweight summary with its cookie provenance.
    pub fn list_summaries(&self) -> Result<Vec<DownloadSummary>, EngineError> {
        let tasks = self.store.list_tasks()?;
        Ok(tasks
            .into_iter()
            .map(|task| {
                let (cookie_source, cookie_count) = self.cookie_provenance(&task);
                let mut summary = DownloadSummary::from(task);
                summary.cookie_source = cookie_source;
                summary.cookie_count = cookie_count;
                summary
            })
            .collect())
    }

    /// The active cookie source name and applicable count for one task.
    fn cookie_provenance(&self, task: &Task) -> (Option<String>, u32) {
        let source = self
            .task_cookie_sources
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&task.id)
            .cloned();

        let url = url::Url::parse(&task.url).ok();
        match source {
            Some(CookieSource::File(path)) => {
                let count = url
                    .as_ref()
                    .and_then(|url| {
                        CookieJar::load(&path)
                            .ok()
                            .map(|jar| jar.matching_count(url))
                    })
                    .unwrap_or(0);
                (Some("file".to_string()), count)
            }
            Some(CookieSource::Browser(cookies)) => {
                let count = url
                    .as_ref()
                    .map(|url| CookieJar::from_browser(cookies).matching_count(url))
                    .unwrap_or(0);
                (Some("browser".to_string()), count)
            }
            None => match self.engine_cookie_file() {
                Some(path) => {
                    let count = url
                        .as_ref()
                        .and_then(|url| {
                            CookieJar::load(&path)
                                .ok()
                                .map(|jar| jar.matching_count(url))
                        })
                        .unwrap_or(0);
                    (Some("file".to_string()), count)
                }
                None => (None, 0),
            },
        }
    }

    /// Resolve and validate the requested destination directory.
    ///
    /// The directory must already exist and be writable, so a task can never be
    /// persisted with a destination that cannot receive its part file.
    fn resolve_destination(
        &self,
        requested: Option<&std::path::Path>,
    ) -> Result<PathBuf, EngineError> {
        let requested = requested
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| self.dest_dir.clone());
        let canonical = std::fs::canonicalize(&requested)
            .map_err(|_| invalid_destination("the destination directory does not exist"))?;
        let metadata = std::fs::metadata(&canonical)
            .map_err(|_| invalid_destination("the destination directory cannot be read"))?;
        if !metadata.is_dir() {
            return Err(invalid_destination("the destination is not a directory"));
        }

        let probe = canonical.join(format!(".oneget-write-probe-{}", uuid::Uuid::new_v4()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
        {
            Ok(file) => {
                drop(file);
                let _ = std::fs::remove_file(&probe);
            }
            Err(_) => {
                return Err(invalid_destination(
                    "the destination directory is not writable",
                ));
            }
        }
        Ok(canonical)
    }

    fn destination_for(&self, task: &Task) -> PathBuf {
        task.destination
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_else(|| self.dest_dir.clone())
    }

    /// Change the concurrency limit without restarting the engine.
    pub fn set_max_concurrent(&self, max: usize) -> Result<(), EngineError> {
        self.max_concurrent.store(max, Ordering::SeqCst);
        self.notify.notify_one();
        Ok(())
    }

    /// Recover interrupted tasks from the durable store and the part files.
    ///
    /// This is synchronous and performs no network access. Interrupted
    /// connecting or downloading tasks are requeued so the download path can
    /// resume them, finalizing tasks go through the idempotent finalize path,
    /// and cancelled tasks have only their own leftover part removed. Paused
    /// and failed tasks keep their parts and state.
    pub fn recover(&self) -> Result<RecoveryReport, EngineError> {
        let mut report = RecoveryReport::default();

        for task in self.store.tasks_by_kind(TaskKind::Http)? {
            let destination = self.destination_for(&task);
            match task.state {
                TaskState::Connecting | TaskState::Downloading => {
                    if part_exists_for_task(&destination, &task) {
                        self.store
                            .transition_task(&task.id, task.state, TaskState::Queued)?;
                        report.requeued += 1;
                    } else {
                        mark_failed(&self.store, &task.id, part_missing());
                        report.failed += 1;
                    }
                }
                TaskState::Finalizing => {
                    if finalize_task(&self.store, &task.id, &destination, None).is_ok() {
                        report.finalized += 1;
                    } else {
                        report.failed += 1;
                    }
                }
                TaskState::Cancelled => {
                    let _ = cancel_task(&self.store, &task.id, &destination);
                    report.cancelled += 1;
                }
                TaskState::Paused => report.paused += 1,
                TaskState::Queued | TaskState::Completed | TaskState::Failed => {}
            }
        }

        Ok(report)
    }

    /// Re-probe paused tasks so a changed remote file or missing part is not
    /// resumed silently. Unchanged tasks stay paused for an explicit resume.
    async fn revalidate_paused(&self) -> Result<(), EngineError> {
        let (client, options) = self.client_and_options()?;
        let hf = self.hf_options();
        let network = NetworkContext {
            client,
            cookies: self.load_cookies().ok().flatten(),
            cookie_error: None,
            stop: None,
            limiters: Vec::new(),
            retry_attempts: options.retry_attempts,
            hf_token: hf.token,
            hf_metadata: hf.metadata,
            hf_connection_cap: hf.connection_cap,
        };

        for task in self.store.tasks_by_kind(TaskKind::Http)? {
            if task.state != TaskState::Paused {
                continue;
            }
            let destination = self.destination_for(&task);
            if !part_exists_for_task(&destination, &task) {
                mark_failed(&self.store, &task.id, part_missing());
                continue;
            }

            let spec = self.spec_for(&task);
            let Ok(outcome) = remote_metadata(&network, &spec, 0).await else {
                // A transient probe failure leaves the task paused rather than
                // discarding recoverable progress.
                continue;
            };

            if task.etag.as_deref() != outcome.etag.as_deref()
                || task.last_modified.as_deref() != outcome.last_modified.as_deref()
                || task.total_bytes != outcome.total_bytes
            {
                mark_failed(
                    &self.store,
                    &task.id,
                    EngineError::new(
                        "FILE_CHANGED",
                        "the remote file changed since the task was paused",
                    ),
                );
            }
        }

        Ok(())
    }

    /// Run until no queued or active tasks remain.
    ///
    /// Startup recovery and paused-task revalidation run first, so a restarted
    /// engine resumes persisted work instead of re-downloading it. Paused tasks
    /// do not consume a slot and do not keep the loop alive. A short idle grace
    /// lets a caller add work immediately after the queue drains, so
    /// pause-then-add and retry flows do not need a new run call.
    pub async fn run(&self) -> Result<(), EngineError> {
        if !self.startup_done.load(Ordering::SeqCst) {
            self.recover()?;
            self.revalidate_paused().await?;
            self.startup_done.store(true, Ordering::SeqCst);
        }

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
        require_http_task(&task)?;
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
        require_http_task(&task)?;
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
        require_http_task(&task)?;
        let destination = self.destination_for(&task);
        match task.state {
            TaskState::Queued | TaskState::Paused | TaskState::Failed | TaskState::Finalizing => {
                cancel_task(&self.store, id, &destination)
            }
            TaskState::Connecting | TaskState::Downloading => match self.existing_control(id) {
                Some(control) => {
                    control.request_cancel();
                    self.notify.notify_one();
                    self.wait_for(id, TaskState::Cancelled).await
                }
                None => cancel_task(&self.store, id, &destination),
            },
            TaskState::Cancelled => Ok(()),
            TaskState::Completed => Err(EngineError::invalid_state(
                "a completed task cannot be cancelled",
            )),
        }
    }

    /// Delete a terminal task record, removing only its own part file.
    ///
    /// Completed final files are never removed here. The record is kept when the
    /// task is still active or when part cleanup fails.
    pub fn delete_record(&self, id: &str) -> Result<(), EngineError> {
        let task = load_task(&self.store, id)?;
        if !matches!(
            task.state,
            TaskState::Completed | TaskState::Failed | TaskState::Cancelled
        ) {
            return Err(EngineError::invalid_state(
                "only a completed, failed, or cancelled record can be deleted",
            ));
        }

        let destination = self.destination_for(&task);
        let cap = CapDir::open(&destination).map_err(platform_error)?;
        let part_name = task_part_name(&task);
        match cap.remove_file(&part_name) {
            Ok(()) => {}
            Err(error) if error.code == "not_found" => {}
            Err(error) => return Err(platform_error(error)),
        }
        self.store.delete_task(id)?;
        self.forget_hf_metadata(id);
        self.forget_cookie_source(id);
        Ok(())
    }

    /// Delete a completed task's final file and then its record.
    ///
    /// A missing final file yields `FILE_MISSING` and keeps the record. Only the
    /// validated final file and the selected record are removed.
    pub fn delete_file(&self, id: &str) -> Result<(), EngineError> {
        let task = load_task(&self.store, id)?;
        if task.state != TaskState::Completed {
            return Err(EngineError::invalid_state(
                "only a completed task's file can be deleted",
            ));
        }

        let destination = self.destination_for(&task);
        let cap = CapDir::open(&destination).map_err(platform_error)?;
        let final_name = match find_completed_final(&cap, &task) {
            Ok(name) => name,
            Err(error) if error.code() == "PART_MISSING" => {
                return Err(EngineError::new(
                    "FILE_MISSING",
                    "the completed task's final file is missing",
                ));
            }
            Err(error) => return Err(error),
        };
        match cap.remove_file(&final_name) {
            Ok(()) => {}
            Err(error) if error.code == "not_found" => {
                return Err(EngineError::new(
                    "FILE_MISSING",
                    "the completed task's final file is missing",
                ));
            }
            Err(error) => return Err(platform_error(error)),
        }
        self.store.delete_task(id)?;
        self.forget_hf_metadata(id);
        self.forget_cookie_source(id);
        Ok(())
    }

    /// Open a completed task's final file with the platform handler.
    ///
    /// A missing final file reports `FILE_MISSING` before any process starts.
    pub fn open_file(&self, id: &str) -> Result<(), EngineError> {
        let task = load_task(&self.store, id)?;
        if task.state != TaskState::Completed {
            return Err(EngineError::invalid_state(
                "only a completed task's file can be opened",
            ));
        }

        let destination = self.destination_for(&task);
        let cap = CapDir::open(&destination).map_err(platform_error)?;
        let final_name = match find_completed_final(&cap, &task) {
            Ok(name) => name,
            Err(error) if error.code() == "PART_MISSING" => {
                return Err(EngineError::new(
                    "FILE_MISSING",
                    "the completed task's final file is missing",
                ));
            }
            Err(error) => return Err(error),
        };
        open_target(&destination.join(final_name)).map_err(platform_error)
    }

    /// Open a task's destination folder with the platform handler.
    pub fn open_folder(&self, id: &str) -> Result<(), EngineError> {
        let task = load_task(&self.store, id)?;
        let destination = self.destination_for(&task);
        open_target(&destination).map_err(platform_error)
    }

    /// Requeue a failed task while preserving its part file.
    pub async fn retry(&self, id: &str) -> Result<(), EngineError> {
        let task = load_task(&self.store, id)?;
        require_http_task(&task)?;
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
        if self.stop_requested() {
            return Ok(None);
        }
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

    fn client_and_options(&self) -> Result<(Client, NetworkOptions), EngineError> {
        let mut cached = self
            .client
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let options = self
            .network_options
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(client) = cached.as_ref() {
            return Ok((client.clone(), options));
        }

        let proxy = self
            .proxy
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        let client = http::build_client_with_network(
            options.connect_timeout,
            options.read_timeout,
            proxy.as_ref(),
            options.user_agent.as_deref(),
        )?;
        *cached = Some(client.clone());
        Ok((client, options))
    }

    pub(crate) fn client(&self) -> Result<Client, EngineError> {
        self.client_and_options().map(|(client, _)| client)
    }

    fn engine_cookie_file(&self) -> Option<PathBuf> {
        self.cookie_file
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn load_cookies(&self) -> Result<Option<CookieJar>, EngineError> {
        match self.engine_cookie_file() {
            Some(path) => CookieJar::load(&path).map(Some),
            None => Ok(None),
        }
    }

    /// Resolve the single cookie source for one task start or resume.
    ///
    /// A task-level file wins, then a legacy engine-wide file, then captured
    /// browser cookies. Sources are never merged.
    fn cookies_for_task(&self, task: &Task) -> (Option<CookieJar>, Option<EngineError>) {
        let source = self
            .task_cookie_sources
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&task.id)
            .cloned();

        if let Some(CookieSource::File(path)) = &source {
            return match CookieJar::load(path) {
                Ok(jar) => (Some(jar), None),
                Err(error) => (None, Some(error)),
            };
        }
        if let Some(path) = self.engine_cookie_file() {
            return match CookieJar::load(&path) {
                Ok(jar) => (Some(jar), None),
                Err(error) => (None, Some(error)),
            };
        }
        match source {
            Some(CookieSource::Browser(cookies)) => (Some(CookieJar::from_browser(cookies)), None),
            _ => (None, None),
        }
    }

    fn spec_for(&self, task: &Task) -> DownloadSpec {
        let mut spec = self
            .specs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&task.id)
            .cloned()
            .unwrap_or_else(|| spec_from_task(task));

        // Configuration that can change between runs stays authoritative in the
        // durable row, while the query-bearing URL and header values remain
        // available only from the in-memory spec.
        spec.filename = task.filename.clone();
        spec.connections = task.connections.max(1);
        if hf::is_resolver_url(&spec.url) {
            let cap = self
                .huggingface
                .read()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .connection_cap
                .max(1);
            spec.connections = spec.connections.min(cap);
        }
        spec.expected_sha256 = task.expected_sha256.clone();
        spec.destination = Some(self.destination_for(task));
        spec
    }

    fn hf_options(&self) -> HuggingFaceOptions {
        self.huggingface
            .read()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Record Hugging Face metadata when a download starts.
    fn record_hf_metadata(&self, task_id: &str, spec: &DownloadSpec) {
        let Ok(Some(parsed)) = hf::parse_hf_url(&spec.url) else {
            return;
        };
        let options = self.hf_options();
        let Some(store) = options.metadata else {
            return;
        };
        let metadata = HfMetadata {
            task_id: task_id.to_string(),
            repo_id: parsed.repo_id,
            file_path: parsed.file_path,
            revision: parsed.revision,
            connection_cap: options.connection_cap,
            effective_connections: spec.connections,
            refresh_count: 0,
            downgraded: false,
        };
        let _ = store.record(&metadata);
    }

    /// Remove a deleted task's Hugging Face metadata when a store is attached.
    fn forget_hf_metadata(&self, task_id: &str) {
        if let Some(store) = self.hf_options().metadata {
            let _ = store.delete(task_id);
        }
    }

    /// Remove a deleted task's memory-only cookie source.
    fn forget_cookie_source(&self, task_id: &str) {
        self.task_cookie_sources
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(task_id);
    }

    fn network_for(&self, task: &Task) -> Result<NetworkContext, EngineError> {
        let (client, options) = self.client_and_options()?;
        let (cookies, cookie_error) = self.cookies_for_task(task);

        let mut limiters = Vec::new();
        if let Some(limiter) = self
            .task_rates
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&task.id)
            .cloned()
        {
            limiters.push(limiter);
        }
        if let Some(limiter) = self
            .global_rate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
        {
            limiters.push(limiter);
        }

        let hf = self.hf_options();
        Ok(NetworkContext {
            client,
            cookies,
            cookie_error,
            stop: self
                .stop
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .clone(),
            limiters,
            retry_attempts: options.retry_attempts,
            hf_token: hf.token,
            hf_metadata: hf.metadata,
            hf_connection_cap: hf.connection_cap,
        })
    }

    fn log(&self, level: LogLevel, event: &str, fields: serde_json::Value) {
        let logger = self
            .logger
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone();
        if let Some(logger) = logger {
            let _ = logger.log(level, event, fields);
        }
    }

    async fn run_task(
        &self,
        task_id: String,
        control: Arc<TaskControl>,
    ) -> Result<(), EngineError> {
        let result = match self.store.task(&task_id)? {
            Some(task) => {
                self.log(
                    LogLevel::Info,
                    "engine.task.started",
                    serde_json::json!({ "task_id": task_id }),
                );
                let spec = self.spec_for(&task);
                self.record_hf_metadata(&task_id, &spec);
                let destination = self.destination_for(&task);
                match self.network_for(&task) {
                    Ok(network) => {
                        download_task_with_network(
                            &self.store,
                            &task_id,
                            &destination,
                            &spec,
                            &control,
                            &network,
                        )
                        .await
                    }
                    Err(error) => Err(error),
                }
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
            Ok(TaskRunOutcome::Completed(outcome)) => {
                self.log(
                    LogLevel::Info,
                    "engine.task.completed",
                    serde_json::json!({
                        "task_id": task_id,
                        "bytes": outcome.total_bytes,
                    }),
                );
                Ok(())
            }
            Ok(TaskRunOutcome::Paused) => {
                self.log(
                    LogLevel::Info,
                    "engine.task.paused",
                    serde_json::json!({ "task_id": task_id }),
                );
                Ok(())
            }
            Ok(TaskRunOutcome::Cancelled) => {
                self.log(
                    LogLevel::Info,
                    "engine.task.cancelled",
                    serde_json::json!({ "task_id": task_id }),
                );
                Ok(())
            }
            Err(error) => {
                let state = self
                    .store
                    .task(&task_id)
                    .ok()
                    .flatten()
                    .map(|task| task.state);
                let stopped = matches!(state, Some(TaskState::Paused | TaskState::Cancelled));
                if !stopped {
                    if let Some(state) = state {
                        if state.can_transition_to(TaskState::Failed) {
                            let _ = self
                                .store
                                .transition_task(&task_id, state, TaskState::Failed);
                        }
                        let _ = self.store.update_task_error(
                            &task_id,
                            error.code(),
                            &error.to_string(),
                        );
                    }
                }
                if stopped {
                    self.log(
                        LogLevel::Info,
                        "engine.task.stopped",
                        serde_json::json!({ "task_id": task_id }),
                    );
                } else {
                    self.log(
                        LogLevel::Warn,
                        "engine.task.failed",
                        serde_json::json!({
                            "task_id": task_id,
                            "error_code": error.code(),
                        }),
                    );
                }
                if error.code() == "DATABASE_ERROR" {
                    return Err(error);
                }
                Ok(())
            }
        }
    }
}

/// Reject a task that the video pipeline owns before any HTTP-engine action.
fn require_http_task(task: &Task) -> Result<(), EngineError> {
    if task.kind != TaskKind::Http {
        return Err(EngineError::new(
            "TASK_KIND_UNSUPPORTED",
            "the HTTP engine only controls HTTP tasks",
        ));
    }
    Ok(())
}

fn active_limiter(bytes_per_second: Option<u64>) -> Option<Arc<RateLimiter>> {
    bytes_per_second
        .filter(|rate| *rate > 0)
        .map(|rate| Arc::new(RateLimiter::new(rate)))
}

fn spec_from_task(task: &Task) -> DownloadSpec {
    DownloadSpec {
        url: task.url.clone(),
        filename: task.filename.clone(),
        connections: task.connections.max(1),
        allow_html: false,
        expected_sha256: task.expected_sha256.clone(),
        extra_headers: Vec::new(),
        destination: task.destination.as_ref().map(PathBuf::from),
    }
}

fn invalid_destination(message: &str) -> EngineError {
    EngineError::new("INVALID_DESTINATION", message)
}

fn platform_error(error: PlatformError) -> EngineError {
    EngineError::new(error.code, error.message)
}
