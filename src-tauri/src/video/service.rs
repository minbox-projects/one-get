//! Long-lived video download service.
//!
//! One dedicated thread owns the task store, the in-memory full source URLs, and
//! a current-thread Tokio runtime. Callers send requests through an unbounded
//! channel and receive replies over a standard channel, mirroring the HTTP
//! engine service. Only one video download runs at a time; a second task stays
//! queued until the active task reaches completed, failed, or cancelled.

use std::cell::RefCell;
use std::collections::HashMap;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use futures_util::future::LocalBoxFuture;
use serde_json::{json, Value};
use tokio::sync::mpsc::UnboundedReceiver;
use url::Url;

use super::control::{ControlAction, DownloadControl};
use super::download::{run_video_task, RunOutcome};
use super::error::VideoError;
use super::files;
use super::metadata::VideoMetadata;
use super::probe::{probe_video, ProbeOptions};
use super::process::TimeoutPolicy;
use super::quality::{build_quality_model, QualityModel};
use crate::engine::error::EngineError;
use crate::engine::http::ProxyConfig;
use crate::engine::state::TaskState;
use crate::kernel::error::KernelError;
use crate::kernel::store::ComponentStore;
use crate::kernel::tools::ToolResolver;
use crate::platform::files::CapDir;
use crate::platform::PlatformError;
use crate::store::{Database, Task, TaskKind, VideoTaskInput};

type ReplySender = Sender<Result<Reply, VideoError>>;
type RunFuture = LocalBoxFuture<'static, RunOutcome>;

/// Configuration for one video service instance.
pub struct VideoServiceConfig {
    /// The durable task store owned by the service thread.
    pub store: Database,
    /// Root directory containing installed component versions.
    pub kernel_root: PathBuf,
    /// Path to the separate component database.
    pub components_db_path: PathBuf,
    /// Destination used when a request does not name one.
    pub default_destination: PathBuf,
    /// Injected process timeout policy.
    pub timeouts: TimeoutPolicy,
    /// Minimum delay between persisted progress snapshots.
    pub progress_persist_interval: Duration,
    /// Effective proxy delivered to every probe and download child.
    pub proxy: Option<ProxyConfig>,
}

/// A confirmed video download request.
#[derive(Debug, Clone)]
pub struct StartVideoRequest {
    /// Full source URL, kept in memory when it must not be persisted.
    pub url: String,
    /// Minimized source URL persisted in the task row.
    pub minimized_url: String,
    /// Desired final filename.
    pub filename: String,
    /// Destination directory, or `None` for the configured default.
    pub destination: Option<PathBuf>,
    /// Selected yt-dlp format selector.
    pub format_selector: String,
    /// Whether the download extracts audio only.
    pub audio_only: bool,
    /// Parsed probe metadata stored with the task.
    pub metadata: Value,
    /// Optional Netscape cookies-file path.
    pub cookies_file: Option<PathBuf>,
    /// Optional credential-free HTTP proxy URL.
    pub proxy_url: Option<String>,
    /// Whether the full source URL may be persisted for a later retry.
    pub full_url_persisted: bool,
}

/// Parsed metadata and ordered quality choices from one probe.
#[derive(Debug, Clone)]
pub struct ProbeResult {
    /// Parsed title, duration, uploader, and formats.
    pub metadata: VideoMetadata,
    /// Deterministic video and audio-only quality choices.
    pub quality: QualityModel,
}

/// The video service request loop and its dedicated thread.
pub struct VideoService {
    sender: tokio::sync::mpsc::UnboundedSender<Command>,
    stopped: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
    shutdown_bound: Duration,
}

impl VideoService {
    /// Start the service, completing interrupted-task recovery before returning.
    pub fn start(config: VideoServiceConfig) -> Result<Self, VideoError> {
        let shutdown_bound = config
            .timeouts
            .shutdown
            .saturating_add(Duration::from_secs(3));
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let thread_stopped = Arc::clone(&stopped);

        let handle = thread::Builder::new()
            .name("oneget-video".to_string())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready_tx.send(Err(VideoError::new(
                            "SERVICE_START_FAILED",
                            format!("the video runtime could not start: {error}"),
                        )));
                        return;
                    }
                };

                let proxy = config.proxy.clone();
                let state = Rc::new(VideoRuntime {
                    config,
                    proxy: RefCell::new(proxy),
                    full_urls: RefCell::new(HashMap::new()),
                });

                match state.config.store.mark_interrupted_video_tasks() {
                    Ok(_) => {
                        if ready_tx.send(Ok(())).is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(store_error(error)));
                        return;
                    }
                }

                runtime.block_on(actor_loop(state, receiver, thread_stopped));
            })
            .map_err(|error| {
                VideoError::new(
                    "SERVICE_START_FAILED",
                    format!("the video service thread could not start: {error}"),
                )
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                sender,
                stopped,
                thread: Mutex::new(Some(handle)),
                shutdown_bound,
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(VideoError::new(
                "SERVICE_START_FAILED",
                "the video service stopped before recovery completed",
            )),
        }
    }

    /// Persist one video task and return its identifier.
    pub fn add(&self, request: StartVideoRequest) -> Result<String, VideoError> {
        self.add_with_connections(request, None)
    }

    /// Persist one video task with its requested connection count.
    pub fn add_with_connections(
        &self,
        request: StartVideoRequest,
        connections: Option<u32>,
    ) -> Result<String, VideoError> {
        self.call(
            move |reply| Command::Add(request, connections, reply),
            expect_id,
        )
    }

    /// Pause a running, queued, or already-paused video task.
    pub fn pause(&self, id: &str) -> Result<(), VideoError> {
        let id = id.to_string();
        self.call(move |reply| Command::Pause(id, reply), expect_unit)
    }

    /// Resume a paused video task.
    pub fn resume(&self, id: &str) -> Result<(), VideoError> {
        let id = id.to_string();
        self.call(move |reply| Command::Resume(id, reply), expect_unit)
    }

    /// Cancel a video task and remove only its task-owned files.
    pub fn cancel(&self, id: &str) -> Result<(), VideoError> {
        let id = id.to_string();
        self.call(move |reply| Command::Cancel(id, reply), expect_unit)
    }

    /// Requeue a failed video task.
    pub fn retry(&self, id: &str) -> Result<(), VideoError> {
        let id = id.to_string();
        self.call(move |reply| Command::Retry(id, reply), expect_unit)
    }

    /// Probe a URL on the service thread after resolving verified tools.
    pub fn probe(
        &self,
        url: &str,
        cookies_file: Option<PathBuf>,
        proxy_url: Option<String>,
    ) -> Result<ProbeResult, VideoError> {
        let url = url.to_string();
        self.call(
            move |reply| Command::Probe(url, cookies_file, proxy_url, reply),
            expect_probe,
        )
    }

    /// Replace the proxy used by later probes and downloads.
    ///
    /// A task that already started keeps the proxy it captured at spawn.
    pub fn set_proxy(&self, proxy: Option<ProxyConfig>) -> Result<(), VideoError> {
        self.call(move |reply| Command::SetProxy(proxy, reply), expect_unit)
    }

    /// Kill active work, persist it paused, and refuse later requests.
    pub fn shutdown(&self) -> Result<(), VideoError> {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return Ok(());
        }

        let (reply_tx, reply_rx) = mpsc::channel();
        if self.sender.send(Command::Shutdown(reply_tx)).is_ok() {
            let _ = reply_rx.recv_timeout(self.shutdown_bound);
        }

        let handle = self
            .thread
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
        Ok(())
    }

    fn call<T>(
        &self,
        make: impl FnOnce(ReplySender) -> Command,
        unpack: fn(Reply) -> Result<T, VideoError>,
    ) -> Result<T, VideoError> {
        if self.stopped.load(Ordering::SeqCst) {
            return Err(service_stopped());
        }

        let (reply_tx, reply_rx) = mpsc::channel();
        self.sender
            .send(make(reply_tx))
            .map_err(|_| service_stopped())?;
        let reply = reply_rx.recv().map_err(|_| service_stopped())?;
        unpack(reply?)
    }
}

pub(crate) struct VideoRuntime {
    pub(crate) config: VideoServiceConfig,
    proxy: RefCell<Option<ProxyConfig>>,
    full_urls: RefCell<HashMap<String, String>>,
}

impl VideoRuntime {
    pub(crate) fn proxy(&self) -> Option<ProxyConfig> {
        self.proxy.borrow().clone()
    }
}

enum Command {
    Add(StartVideoRequest, Option<u32>, ReplySender),
    Probe(String, Option<PathBuf>, Option<String>, ReplySender),
    Pause(String, ReplySender),
    Resume(String, ReplySender),
    Cancel(String, ReplySender),
    Retry(String, ReplySender),
    SetProxy(Option<ProxyConfig>, ReplySender),
    Shutdown(Sender<Result<(), VideoError>>),
}

enum Reply {
    Unit,
    Id(String),
    Probe(Box<ProbeResult>),
}

struct ActiveRun {
    task_id: String,
    control: DownloadControl,
    future: Option<RunFuture>,
}

fn expect_unit(reply: Reply) -> Result<(), VideoError> {
    match reply {
        Reply::Unit => Ok(()),
        _ => Err(unexpected_reply()),
    }
}

fn expect_id(reply: Reply) -> Result<String, VideoError> {
    match reply {
        Reply::Id(id) => Ok(id),
        _ => Err(unexpected_reply()),
    }
}

fn expect_probe(reply: Reply) -> Result<ProbeResult, VideoError> {
    match reply {
        Reply::Probe(result) => Ok(*result),
        _ => Err(unexpected_reply()),
    }
}

async fn actor_loop(
    runtime: Rc<VideoRuntime>,
    mut receiver: UnboundedReceiver<Command>,
    stopped: Arc<AtomicBool>,
) {
    let mut active: Option<ActiveRun> = None;
    let mut want_run = false;

    loop {
        if active.is_none() && want_run && !stopped.load(Ordering::SeqCst) {
            match runtime.config.store.claim_oldest_queued_video_task() {
                Ok(Some(task)) => {
                    let id = task.id.clone();
                    let control = DownloadControl::new();
                    let future =
                        Box::pin(run_video_task(Rc::clone(&runtime), task, control.clone()));
                    active = Some(ActiveRun {
                        task_id: id,
                        control,
                        future: Some(future),
                    });
                }
                Ok(None) => want_run = false,
                Err(_) => want_run = false,
            }
        }

        let Some(mut run) = active.take() else {
            match receiver.recv().await {
                Some(Command::Shutdown(reply)) => {
                    let _ = reply.send(Ok(()));
                    break;
                }
                Some(command) => {
                    if !handle_idle_command(&runtime, command, &mut want_run) {
                        break;
                    }
                }
                None => break,
            }
            continue;
        };

        let mut future = run
            .future
            .take()
            .expect("an active video run always carries a future");
        tokio::select! {
            outcome = &mut future => {
                run.future = Some(future);
                want_run = finish_run(&runtime, &run, outcome, &stopped).await;
            }
            incoming = receiver.recv() => {
                run.future = Some(future);
                match incoming {
                    Some(Command::Shutdown(reply)) => {
                        run.control.request(ControlAction::Pause);
                        let outcome = await_active(&runtime, &mut run).await;
                        let _ = settle_interrupted(&runtime, &run.task_id, outcome, ControlAction::Pause).await;
                        let _ = reply.send(Ok(()));
                        break;
                    }
                    Some(command) => {
                        if handle_active_command(&runtime, &mut run, command, &mut want_run, &stopped).await {
                            active = Some(run);
                        }
                    }
                    None => break,
                }
            }
        }
    }

    stopped.store(true, Ordering::SeqCst);
}

async fn finish_run(
    runtime: &Rc<VideoRuntime>,
    run: &ActiveRun,
    outcome: RunOutcome,
    stopped: &AtomicBool,
) -> bool {
    match outcome {
        RunOutcome::Completed | RunOutcome::Failed => !stopped.load(Ordering::SeqCst),
        RunOutcome::Cancelled => {
            let action = run.control.reason().unwrap_or(ControlAction::Cancel);
            let _ = settle_interrupted(runtime, &run.task_id, RunOutcome::Cancelled, action).await;
            matches!(action, ControlAction::Cancel) && !stopped.load(Ordering::SeqCst)
        }
    }
}

async fn await_active(runtime: &Rc<VideoRuntime>, run: &mut ActiveRun) -> RunOutcome {
    let Some(mut future) = run.future.take() else {
        return RunOutcome::Failed;
    };
    let bound = runtime
        .config
        .timeouts
        .shutdown
        .saturating_add(Duration::from_secs(2));
    match tokio::time::timeout(bound, &mut future).await {
        Ok(outcome) => outcome,
        Err(_) => {
            // Dropping the future drops the child guard, which signals the whole
            // process tree synchronously.
            drop(future);
            RunOutcome::Cancelled
        }
    }
}

async fn handle_active_command(
    runtime: &Rc<VideoRuntime>,
    run: &mut ActiveRun,
    command: Command,
    want_run: &mut bool,
    stopped: &AtomicBool,
) -> bool {
    match command {
        Command::Pause(id, reply) if id == run.task_id => {
            run.control.request(ControlAction::Pause);
            let outcome = await_active(runtime, run).await;
            let result = match outcome {
                RunOutcome::Cancelled => {
                    settle_interrupted(
                        runtime,
                        &run.task_id,
                        RunOutcome::Cancelled,
                        ControlAction::Pause,
                    )
                    .await
                }
                RunOutcome::Failed => Ok(()),
                RunOutcome::Completed => {
                    Err(invalid_state("a completed video task cannot be paused"))
                }
            };
            let _ = reply.send(result.map(|()| Reply::Unit));
            *want_run = false;
            false
        }
        Command::Cancel(id, reply) if id == run.task_id => {
            run.control.request(ControlAction::Cancel);
            let outcome = await_active(runtime, run).await;
            let result = match outcome {
                RunOutcome::Completed => {
                    Err(invalid_state("a completed video task cannot be cancelled"))
                }
                RunOutcome::Failed | RunOutcome::Cancelled => {
                    settle_interrupted(
                        runtime,
                        &run.task_id,
                        RunOutcome::Cancelled,
                        ControlAction::Cancel,
                    )
                    .await
                }
            };
            if result.is_ok() {
                *want_run = !stopped.load(Ordering::SeqCst);
            }
            let _ = reply.send(result.map(|()| Reply::Unit));
            false
        }
        command => handle_idle_command(runtime, command, want_run),
    }
}

fn handle_idle_command(runtime: &Rc<VideoRuntime>, command: Command, want_run: &mut bool) -> bool {
    match command {
        Command::Add(request, connections, reply) => {
            let result = add_request(runtime, request, connections);
            if result.is_ok() {
                *want_run = true;
            }
            let _ = reply.send(result.map(Reply::Id));
        }
        Command::Probe(url, cookies_file, proxy_url, reply) => {
            if let Err(error) = validate_per_video_proxy(proxy_url.as_deref()) {
                let _ = reply.send(Err(error));
                return true;
            }
            let tools = resolve_execution_tools(&runtime.config);
            match tools {
                Ok(tools) => {
                    let timeout = runtime.config.timeouts.probe;
                    let proxy = runtime.proxy();
                    tokio::spawn(async move {
                        let options = ProbeOptions {
                            yt_dlp: tools.yt_dlp,
                            deno: tools.deno,
                            cookies_file,
                            proxy_url,
                            proxy,
                            timeout,
                        };
                        let result = probe_video(&url, &options).await.map(|metadata| {
                            let quality = build_quality_model(&metadata);
                            Reply::Probe(Box::new(ProbeResult { metadata, quality }))
                        });
                        let _ = reply.send(result);
                    });
                }
                Err(error) => {
                    let _ = reply.send(Err(error));
                }
            }
        }
        Command::Pause(id, reply) => {
            let _ = reply.send(pause_idle(runtime, &id).map(|()| Reply::Unit));
        }
        Command::Resume(id, reply) => {
            let result = resume_request(runtime, &id);
            if result.is_ok() {
                *want_run = true;
            }
            let _ = reply.send(result.map(|()| Reply::Unit));
        }
        Command::Cancel(id, reply) => {
            let _ = reply.send(cancel_idle(runtime, &id).map(|()| Reply::Unit));
        }
        Command::Retry(id, reply) => {
            let result = retry_request(runtime, &id);
            if result.is_ok() {
                *want_run = true;
            }
            let _ = reply.send(result.map(|()| Reply::Unit));
        }
        Command::SetProxy(proxy, reply) => {
            *runtime.proxy.borrow_mut() = proxy;
            let _ = reply.send(Ok(Reply::Unit));
        }
        Command::Shutdown(reply) => {
            let _ = reply.send(Ok(()));
            return false;
        }
    }
    true
}

async fn settle_interrupted(
    runtime: &Rc<VideoRuntime>,
    id: &str,
    outcome: RunOutcome,
    action: ControlAction,
) -> Result<(), VideoError> {
    match (outcome, action) {
        (RunOutcome::Completed, _) => Err(invalid_state(
            "a completed video task cannot be interrupted",
        )),
        (RunOutcome::Failed, ControlAction::Pause) => Ok(()),
        (_, ControlAction::Pause) => transition_to(runtime, id, TaskState::Paused),
        (_, ControlAction::Cancel) => {
            cleanup_task(runtime, id)?;
            transition_to(runtime, id, TaskState::Cancelled)
        }
    }
}

fn add_request(
    runtime: &Rc<VideoRuntime>,
    request: StartVideoRequest,
    connections: Option<u32>,
) -> Result<String, VideoError> {
    validate_per_video_proxy(request.proxy_url.as_deref())?;

    let requirements = if request.audio_only {
        vec![
            crate::kernel::tools::ToolRequirement::YtDlp,
            crate::kernel::tools::ToolRequirement::Ffmpeg,
        ]
    } else {
        vec![
            crate::kernel::tools::ToolRequirement::YtDlp,
            crate::kernel::tools::ToolRequirement::Ffmpeg,
            crate::kernel::tools::ToolRequirement::Ffprobe,
        ]
    };

    let components =
        ComponentStore::open(&runtime.config.components_db_path).map_err(kernel_error)?;
    let kernel_root = CapDir::open(&runtime.config.kernel_root).map_err(platform_error)?;
    ToolResolver::new(&components, &kernel_root)
        .ensure(&requirements)
        .map_err(kernel_error)?;

    let destination = request
        .destination
        .clone()
        .unwrap_or_else(|| runtime.config.default_destination.clone());
    let input = VideoTaskInput {
        url: request.minimized_url.clone(),
        filename: request.filename.clone(),
        destination: Some(destination.to_string_lossy().into_owned()),
        format_selector: request.format_selector.clone(),
        cookies_file: request
            .cookies_file
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
        proxy_url: request.proxy_url.clone(),
        metadata: request.metadata.clone(),
        full_url_persisted: request.full_url_persisted,
    };

    let id = runtime
        .config
        .store
        .create_video_task(&input)
        .map_err(store_error)?;

    let mut patch = json!({ "audioOnly": request.audio_only });
    if request.full_url_persisted {
        if let Value::Object(object) = &mut patch {
            object.insert("fullUrl".to_string(), Value::String(request.url.clone()));
        }
    }
    if let Err(error) = runtime.config.store.patch_video_json(&id, &patch) {
        let _ = runtime.config.store.delete_task(&id);
        return Err(store_error(error));
    }

    if let Some(connections) = connections {
        if let Err(error) = runtime.config.store.record_connections(&id, connections) {
            let _ = runtime.config.store.delete_task(&id);
            return Err(store_error(error));
        }
    }

    runtime
        .full_urls
        .borrow_mut()
        .insert(id.clone(), request.url);
    Ok(id)
}

fn pause_idle(runtime: &Rc<VideoRuntime>, id: &str) -> Result<(), VideoError> {
    let task = load_video_task(runtime, id)?;
    match task.state {
        TaskState::Queued => transition_to(runtime, id, TaskState::Paused),
        TaskState::Paused => Ok(()),
        TaskState::Failed | TaskState::Cancelled | TaskState::Completed => Err(invalid_state(
            "only a queued, connecting, downloading, or paused video task can be paused",
        )),
        _ => Err(invalid_state(
            "the active video task must be paused through the running service",
        )),
    }
}

fn resume_request(runtime: &Rc<VideoRuntime>, id: &str) -> Result<(), VideoError> {
    let task = load_video_task(runtime, id)?;
    let _ = resolve_full_url(runtime, &task)?;
    if task.state != TaskState::Paused {
        return Err(invalid_state("only a paused video task can be resumed"));
    }
    transition_to(runtime, id, TaskState::Queued)
}

fn retry_request(runtime: &Rc<VideoRuntime>, id: &str) -> Result<(), VideoError> {
    let task = load_video_task(runtime, id)?;
    // A task paused before a restart is retried from its preserved temporary
    // directory, so the full source URL must be available before the state
    // check can change anything.
    let _ = resolve_full_url(runtime, &task)?;
    if !matches!(task.state, TaskState::Failed | TaskState::Paused) {
        return Err(invalid_state(
            "only a failed or paused video task can be retried",
        ));
    }
    transition_to(runtime, id, TaskState::Queued)
}

fn cancel_idle(runtime: &Rc<VideoRuntime>, id: &str) -> Result<(), VideoError> {
    let task = load_video_task(runtime, id)?;
    match task.state {
        TaskState::Completed => Err(invalid_state("a completed video task cannot be cancelled")),
        TaskState::Cancelled => Ok(()),
        TaskState::Connecting | TaskState::Downloading | TaskState::Finalizing => Err(
            invalid_state("the active video task must be cancelled through the running service"),
        ),
        _ => {
            cleanup_task(runtime, id)?;
            transition_to(runtime, id, TaskState::Cancelled)
        }
    }
}

fn cleanup_task(runtime: &Rc<VideoRuntime>, id: &str) -> Result<(), VideoError> {
    let task = load_video_task(runtime, id)?;
    let destination = task
        .destination
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| runtime.config.default_destination.clone());
    let temp = files::temp_dir(task.video.as_ref(), &destination, id);
    files::remove_temp_dir(&temp);
    Ok(())
}

pub(crate) fn resolve_full_url(runtime: &VideoRuntime, task: &Task) -> Result<String, VideoError> {
    if let Some(url) = runtime.full_urls.borrow().get(&task.id) {
        return Ok(url.clone());
    }
    let video = task.video.as_ref();
    let persisted = video
        .and_then(|record| record.get("fullUrlPersisted"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !persisted {
        return Err(VideoError::new(
            "VIDEO_URL_UNAVAILABLE",
            "the full source URL was not persisted, so the task cannot be retried after a restart",
        ));
    }
    let full = video
        .and_then(|record| record.get("fullUrl"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| task.url.clone());
    Ok(full)
}

pub(crate) struct ExecutionTools {
    pub(crate) yt_dlp: PathBuf,
    pub(crate) ffmpeg_dir: PathBuf,
    pub(crate) deno: Option<PathBuf>,
}

pub(crate) fn resolve_execution_tools(
    config: &VideoServiceConfig,
) -> Result<ExecutionTools, VideoError> {
    let components = ComponentStore::open(&config.components_db_path).map_err(kernel_error)?;
    let kernel_root = CapDir::open(&config.kernel_root).map_err(platform_error)?;
    let resolved = ToolResolver::new(&components, &kernel_root)
        .resolve()
        .map_err(kernel_error)?;
    Ok(ExecutionTools {
        yt_dlp: resolved.yt_dlp,
        ffmpeg_dir: resolved.ffmpeg_dir,
        deno: resolved.deno,
    })
}

pub(crate) fn read_video_string(task: &Task, key: &str) -> Option<String> {
    task.video
        .as_ref()
        .and_then(|record| record.get(key))
        .and_then(Value::as_str)
        .map(str::to_string)
}

pub(crate) fn read_video_flag(task: &Task, key: &str) -> bool {
    task.video
        .as_ref()
        .and_then(|record| record.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub(crate) fn task_destination(runtime: &VideoRuntime, task: &Task) -> PathBuf {
    task.destination
        .as_deref()
        .map(PathBuf::from)
        .unwrap_or_else(|| runtime.config.default_destination.clone())
}

pub(crate) fn transition_to(
    runtime: &Rc<VideoRuntime>,
    id: &str,
    target: TaskState,
) -> Result<(), VideoError> {
    let task = load_video_task(runtime, id)?;
    if task.state == target {
        return Ok(());
    }
    if !task.state.can_transition_to(target) {
        return Err(invalid_state(
            "the requested video task transition is not permitted",
        ));
    }
    runtime
        .config
        .store
        .transition_task(id, task.state, target)
        .map_err(store_error)
}

pub(crate) fn fail_video_task(
    runtime: &Rc<VideoRuntime>,
    id: &str,
    code: &str,
    message: &str,
) -> RunOutcome {
    let _ = transition_to(runtime, id, TaskState::Failed);
    let _ = runtime.config.store.update_task_error(id, code, message);
    RunOutcome::Failed
}

fn load_video_task(runtime: &Rc<VideoRuntime>, id: &str) -> Result<Task, VideoError> {
    let task = runtime
        .config
        .store
        .task(id)
        .map_err(store_error)?
        .ok_or_else(|| VideoError::new("TASK_NOT_FOUND", "the requested task does not exist"))?;
    if task.kind != TaskKind::Video {
        return Err(VideoError::new(
            "INVALID_KIND",
            "the requested task is not a video task",
        ));
    }
    Ok(task)
}

pub(crate) fn store_error(error: EngineError) -> VideoError {
    VideoError::new(error.code(), error.to_string())
}

fn kernel_error(error: KernelError) -> VideoError {
    VideoError::new(error.code(), error.message())
}

fn platform_error(error: PlatformError) -> VideoError {
    VideoError::new(error.code, error.message)
}

fn invalid_state(message: impl Into<String>) -> VideoError {
    VideoError::new("INVALID_STATE", message)
}

/// The per-video input stays a credential-free HTTP or HTTPS proxy.
///
/// Settings proxies may use SOCKS5; a task-level override keeps the narrower
/// input contract its command surface already exposes.
fn validate_per_video_proxy(proxy: Option<&str>) -> Result<(), VideoError> {
    let Some(value) = proxy else {
        return Ok(());
    };
    let validated =
        crate::engine::http::validate_proxy_url(value).map_err(|_| invalid_per_video_proxy())?;
    let url = Url::parse(&validated).map_err(|_| invalid_per_video_proxy())?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(invalid_per_video_proxy());
    }
    Ok(())
}

fn invalid_per_video_proxy() -> VideoError {
    VideoError::new(
        "INVALID_PROXY_URL",
        "the per-video proxy must be a credential-free HTTP or HTTPS URL",
    )
}

fn service_stopped() -> VideoError {
    VideoError::new("SERVICE_STOPPED", "the video service has stopped")
}

fn unexpected_reply() -> VideoError {
    VideoError::new(
        "SERVICE_ERROR",
        "the video service returned an unexpected response",
    )
}
