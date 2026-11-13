//! Long-lived download engine service.
//!
//! The engine and its SQLite store are owned by one dedicated thread with a
//! current-thread Tokio runtime, because neither type is safe to share across
//! threads. Every caller request is routed through a channel, and the engine
//! run loop is polled concurrently with those requests so pause, resume, cancel,
//! and probe work while downloads are progressing. The run loop is restarted
//! when new work arrives after it idles.

use std::future::Future;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use futures_util::future::LocalBoxFuture;
use tokio::sync::mpsc::UnboundedReceiver;

use super::error::EngineError;
use super::http;
use super::probe::{probe as probe_url, ProbeOptions, ProbeOutcome};
use super::scheduler::{Engine, EngineOptions};
use super::state::TaskState;
use super::worker::DownloadSpec;
use crate::store::{Database, Segment, Task};

type RunFuture = LocalBoxFuture<'static, Result<(), EngineError>>;
type ReplySender = Sender<Result<Reply, EngineError>>;

/// A download request accepted by the engine service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddDownload {
    /// Remote HTTP or HTTPS URL.
    pub url: String,
    /// Requested final filename before sanitization.
    pub filename: String,
    /// Destination directory, or `None` to use the service default.
    pub destination: Option<PathBuf>,
    /// Requested parallel connection count.
    pub connections: u32,
    /// Optional per-task rate limit in bytes per second.
    pub rate_limit: Option<u64>,
    /// Optional expected SHA-256 of the finished file.
    pub expected_sha256: Option<String>,
    /// Accept an HTML response as an ordinary file.
    pub allow_html: bool,
    /// Additional request headers.
    pub extra_headers: Vec<(String, String)>,
}

/// A probe request accepted by the engine service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeRequest {
    /// Remote HTTP or HTTPS URL.
    pub url: String,
    /// Accept an HTML response instead of reporting `HTML_RESPONSE`.
    pub allow_html: bool,
    /// Additional request headers.
    pub extra_headers: Vec<(String, String)>,
}

/// The engine request loop owns the engine and answers one caller at a time.
pub struct EngineService {
    sender: tokio::sync::mpsc::UnboundedSender<Command>,
    stopped: Arc<AtomicBool>,
    thread: Mutex<Option<JoinHandle<()>>>,
}

impl EngineService {
    /// Open the engine over an existing store and start its dedicated thread.
    ///
    /// Recovery runs before this function returns, so interrupted tasks are
    /// visible and scheduled immediately.
    pub fn start(
        store: Database,
        default_destination: PathBuf,
        options: EngineOptions,
    ) -> Result<Self, EngineError> {
        let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
        let (ready_tx, ready_rx) = mpsc::channel();
        let stopped = Arc::new(AtomicBool::new(false));
        let thread_stopped = Arc::clone(&stopped);

        let handle = thread::Builder::new()
            .name("oneget-engine".to_string())
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        let _ = ready_tx.send(Err(EngineError::new(
                            "SERVICE_START_FAILED",
                            format!("the engine runtime could not start: {error}"),
                        )));
                        return;
                    }
                };

                let engine = Rc::new(Engine::new(store, default_destination, options));
                match engine.recover() {
                    Ok(_) => {
                        if ready_tx.send(Ok(())).is_err() {
                            return;
                        }
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                        return;
                    }
                }

                runtime.block_on(actor_loop(engine, receiver, thread_stopped));
            })
            .map_err(|error| {
                EngineError::new(
                    "SERVICE_START_FAILED",
                    format!("the engine thread could not start: {error}"),
                )
            })?;

        match ready_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                sender,
                stopped,
                thread: Mutex::new(Some(handle)),
            }),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(EngineError::new(
                "SERVICE_START_FAILED",
                "the engine thread stopped before recovery completed",
            )),
        }
    }

    /// Load every stored task.
    pub fn list(&self) -> Result<Vec<Task>, EngineError> {
        self.call(Command::List, expect_tasks)
    }

    /// Load one task's segment plan.
    pub fn segments(&self, id: &str) -> Result<Vec<Segment>, EngineError> {
        let id = id.to_string();
        self.call(move |reply| Command::Segments(id, reply), expect_segments)
    }

    /// Probe a remote URL for download metadata.
    pub fn probe(&self, request: ProbeRequest) -> Result<ProbeOutcome, EngineError> {
        self.call(move |reply| Command::Probe(request, reply), expect_probe)
    }

    /// Create a queued download task and return its identifier.
    pub fn add(&self, request: AddDownload) -> Result<String, EngineError> {
        self.call(move |reply| Command::Add(request, reply), expect_id)
    }

    /// Pause a queued, connecting, or downloading task.
    pub fn pause(&self, id: &str) -> Result<(), EngineError> {
        let id = id.to_string();
        self.call(move |reply| Command::Pause(id, reply), expect_unit)
    }

    /// Resume a paused task.
    pub fn resume(&self, id: &str) -> Result<(), EngineError> {
        let id = id.to_string();
        self.call(move |reply| Command::Resume(id, reply), expect_unit)
    }

    /// Cancel a task and remove only its own part file.
    pub fn cancel(&self, id: &str) -> Result<(), EngineError> {
        let id = id.to_string();
        self.call(move |reply| Command::Cancel(id, reply), expect_unit)
    }

    /// Requeue a failed task while preserving its part file.
    pub fn retry(&self, id: &str) -> Result<(), EngineError> {
        let id = id.to_string();
        self.call(move |reply| Command::Retry(id, reply), expect_unit)
    }

    /// Delete a terminal task record, keeping any completed final file.
    pub fn delete_record(&self, id: &str) -> Result<(), EngineError> {
        let id = id.to_string();
        self.call(move |reply| Command::DeleteRecord(id, reply), expect_unit)
    }

    /// Delete a completed task's final file and its record.
    pub fn delete_file(&self, id: &str) -> Result<(), EngineError> {
        let id = id.to_string();
        self.call(move |reply| Command::DeleteFile(id, reply), expect_unit)
    }

    /// Open a completed task's final file with the platform handler.
    pub fn open_file(&self, id: &str) -> Result<(), EngineError> {
        let id = id.to_string();
        self.call(move |reply| Command::OpenFile(id, reply), expect_unit)
    }

    /// Open a task's recorded destination folder with the platform handler.
    pub fn open_folder(&self, id: &str) -> Result<(), EngineError> {
        let id = id.to_string();
        self.call(move |reply| Command::OpenFolder(id, reply), expect_unit)
    }

    /// Apply the concurrency and global rate limits to the live engine.
    pub fn apply_settings(
        &self,
        max_concurrent: usize,
        global_rate_limit: Option<u64>,
    ) -> Result<(), EngineError> {
        self.call(
            move |reply| Command::ApplySettings {
                max_concurrent,
                global_rate_limit,
                reply,
            },
            expect_unit,
        )
    }

    /// Pause active downloads, stop the request loop, and refuse later work.
    pub fn shutdown(&self) {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return;
        }

        let (reply_tx, reply_rx) = mpsc::channel();
        if self.sender.send(Command::Shutdown(reply_tx)).is_ok() {
            let _ = reply_rx.recv_timeout(Duration::from_secs(30));
        }

        let handle = self
            .thread
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(handle) = handle {
            let _ = handle.join();
        }
    }

    fn call<T>(
        &self,
        make: impl FnOnce(ReplySender) -> Command,
        unpack: fn(Reply) -> Result<T, EngineError>,
    ) -> Result<T, EngineError> {
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

enum Command {
    List(ReplySender),
    Segments(String, ReplySender),
    Probe(ProbeRequest, ReplySender),
    Add(AddDownload, ReplySender),
    Pause(String, ReplySender),
    Resume(String, ReplySender),
    Cancel(String, ReplySender),
    Retry(String, ReplySender),
    DeleteRecord(String, ReplySender),
    DeleteFile(String, ReplySender),
    OpenFile(String, ReplySender),
    OpenFolder(String, ReplySender),
    ApplySettings {
        max_concurrent: usize,
        global_rate_limit: Option<u64>,
        reply: ReplySender,
    },
    Shutdown(Sender<Result<(), EngineError>>),
}

enum Reply {
    Unit,
    Tasks(Vec<Task>),
    Segments(Vec<Segment>),
    Probe(ProbeOutcome),
    Id(String),
}

fn expect_unit(reply: Reply) -> Result<(), EngineError> {
    match reply {
        Reply::Unit => Ok(()),
        _ => Err(unexpected_reply()),
    }
}

fn expect_tasks(reply: Reply) -> Result<Vec<Task>, EngineError> {
    match reply {
        Reply::Tasks(tasks) => Ok(tasks),
        _ => Err(unexpected_reply()),
    }
}

fn expect_segments(reply: Reply) -> Result<Vec<Segment>, EngineError> {
    match reply {
        Reply::Segments(segments) => Ok(segments),
        _ => Err(unexpected_reply()),
    }
}

fn expect_probe(reply: Reply) -> Result<ProbeOutcome, EngineError> {
    match reply {
        Reply::Probe(outcome) => Ok(outcome),
        _ => Err(unexpected_reply()),
    }
}

fn expect_id(reply: Reply) -> Result<String, EngineError> {
    match reply {
        Reply::Id(id) => Ok(id),
        _ => Err(unexpected_reply()),
    }
}

async fn actor_loop(
    engine: Rc<Engine>,
    mut receiver: UnboundedReceiver<Command>,
    stopped: Arc<AtomicBool>,
) {
    let mut run: Option<RunFuture> = Some(Box::pin(run_engine(Rc::clone(&engine))));

    loop {
        let command = match run.take() {
            Some(mut current) => {
                tokio::select! {
                    result = &mut current => {
                        if let Err(error) = result {
                            eprintln!("download engine loop stopped: {error}");
                        }
                        match receiver.recv().await {
                            Some(command) => command,
                            None => break,
                        }
                    }
                    incoming = receiver.recv() => {
                        run = Some(current);
                        match incoming {
                            Some(command) => command,
                            None => break,
                        }
                    }
                }
            }
            None => match receiver.recv().await {
                Some(command) => command,
                None => break,
            },
        };

        match command {
            Command::Shutdown(reply) => {
                let result = shutdown_engine(&engine, &mut run).await;
                let _ = reply.send(result);
                break;
            }
            other => handle_command(&engine, &mut run, other).await,
        }
    }

    stopped.store(true, Ordering::SeqCst);
}

async fn run_engine(engine: Rc<Engine>) -> Result<(), EngineError> {
    engine.run().await
}

async fn handle_command(engine: &Rc<Engine>, run: &mut Option<RunFuture>, command: Command) {
    match command {
        Command::List(reply) => {
            let _ = reply.send(engine.store().list_tasks().map(Reply::Tasks));
        }
        Command::Segments(id, reply) => {
            let result = engine.store().segments_for_task(&id).map(Reply::Segments);
            let _ = reply.send(result);
        }
        Command::Probe(request, reply) => {
            let result = probe_request(request).await;
            let _ = reply.send(result.map(Reply::Probe));
        }
        Command::Add(request, reply) => {
            let result = add_request(engine, request);
            if result.is_ok() {
                ensure_run(engine, run);
            }
            let _ = reply.send(result.map(Reply::Id));
        }
        Command::Pause(id, reply) => {
            let result = await_with_run(run, engine.pause(&id)).await;
            let _ = reply.send(result.map(|()| Reply::Unit));
        }
        Command::Resume(id, reply) => {
            let result = await_with_run(run, engine.resume(&id)).await;
            if result.is_ok() {
                ensure_run(engine, run);
            }
            let _ = reply.send(result.map(|()| Reply::Unit));
        }
        Command::Cancel(id, reply) => {
            let result = await_with_run(run, engine.cancel(&id)).await;
            let _ = reply.send(result.map(|()| Reply::Unit));
        }
        Command::Retry(id, reply) => {
            let result = await_with_run(run, engine.retry(&id)).await;
            if result.is_ok() {
                ensure_run(engine, run);
            }
            let _ = reply.send(result.map(|()| Reply::Unit));
        }
        Command::DeleteRecord(id, reply) => {
            let _ = reply.send(engine.delete_record(&id).map(|()| Reply::Unit));
        }
        Command::DeleteFile(id, reply) => {
            let _ = reply.send(engine.delete_file(&id).map(|()| Reply::Unit));
        }
        Command::OpenFile(id, reply) => {
            let _ = reply.send(engine.open_file(&id).map(|()| Reply::Unit));
        }
        Command::OpenFolder(id, reply) => {
            let _ = reply.send(engine.open_folder(&id).map(|()| Reply::Unit));
        }
        Command::ApplySettings {
            max_concurrent,
            global_rate_limit,
            reply,
        } => {
            let result = engine.set_max_concurrent(max_concurrent);
            engine.set_global_rate_limit(global_rate_limit);
            let _ = reply.send(result.map(|()| Reply::Unit));
        }
        Command::Shutdown(_) => {}
    }
}

fn add_request(engine: &Engine, request: AddDownload) -> Result<String, EngineError> {
    let AddDownload {
        url,
        filename,
        destination,
        connections,
        rate_limit,
        expected_sha256,
        allow_html,
        extra_headers,
    } = request;

    let url = http::validate_url(&url)?;
    http::validate_extra_headers(&extra_headers)?;
    if !(1..=64).contains(&connections) {
        return Err(EngineError::new(
            "INVALID_CONNECTIONS",
            "the connection count must be between 1 and 64",
        ));
    }

    let spec = DownloadSpec {
        url,
        filename,
        connections,
        allow_html,
        expected_sha256,
        extra_headers,
        destination,
    };
    let id = engine.add_task(spec)?;
    if rate_limit.is_some() {
        engine.set_task_rate_limit(&id, rate_limit);
    }
    Ok(id)
}

async fn probe_request(request: ProbeRequest) -> Result<ProbeOutcome, EngineError> {
    let url = http::validate_url(&request.url)?;
    http::validate_extra_headers(&request.extra_headers)?;
    let options = ProbeOptions {
        allow_html: request.allow_html,
        extra_headers: request.extra_headers,
    };
    probe_url(&url, &options).await
}

fn ensure_run(engine: &Rc<Engine>, run: &mut Option<RunFuture>) {
    if run.is_none() {
        *run = Some(Box::pin(run_engine(Rc::clone(engine))));
    }
}

async fn await_with_run<T, F>(run: &mut Option<RunFuture>, future: F) -> Result<T, EngineError>
where
    F: Future<Output = Result<T, EngineError>>,
{
    tokio::pin!(future);
    loop {
        match run.take() {
            Some(mut current) => {
                tokio::select! {
                    result = &mut future => {
                        *run = Some(current);
                        return result;
                    }
                    ended = &mut current => {
                        ended?;
                    }
                }
            }
            None => return future.await,
        }
    }
}

async fn shutdown_engine(
    engine: &Rc<Engine>,
    run: &mut Option<RunFuture>,
) -> Result<(), EngineError> {
    let tasks = engine.store().list_tasks()?;
    for task in tasks {
        if matches!(task.state, TaskState::Connecting | TaskState::Downloading) {
            // Ignore a task that finished or changed state while shutting down;
            // its own persisted state remains authoritative.
            let _ = await_with_run(run, engine.pause(&task.id)).await;
        }
    }
    *run = None;
    Ok(())
}

fn service_stopped() -> EngineError {
    EngineError::new("SERVICE_STOPPED", "the download engine service has stopped")
}

fn unexpected_reply() -> EngineError {
    EngineError::new(
        "SERVICE_ERROR",
        "the download engine service returned an unexpected response",
    )
}
