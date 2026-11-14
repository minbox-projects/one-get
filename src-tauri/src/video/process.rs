//! Guarded process runner with bounded output, line streaming, and injected
//! timeouts.
//!
//! Every spawn goes through [`ChildGuard`], so children run without a shell in
//! a Unix process group or a Windows Job Object and the whole tree is killed on
//! timeout or drop. Standard output and standard error are drained concurrently
//! into a combined 2 MiB capture plus a rolling 8 KiB tail per stream, so a
//! flooding child can never block on a full pipe.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::time::Instant;

use super::control::DownloadControl;
use super::error::VideoError;
use crate::platform::process::ChildGuard;

/// Maximum number of bytes retained across both output streams.
const COMBINED_OUTPUT_CAP: usize = 2 * 1024 * 1024;
/// Rolling tail retained per stream beyond the combined cap.
const STREAM_TAIL: usize = 8 * 1024;
/// Chunk size used to drain each pipe.
const READ_CHUNK: usize = 64 * 1024;
/// Upper bound for an unterminated line held between reads.
const LINE_CARRY_LIMIT: usize = 64 * 1024;
/// Delay between non-blocking exit checks.
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Maximum size of the child-output tail included in an error message.
const ERROR_TAIL: usize = 2 * 1024;

/// How a subprocess should be invoked.
#[derive(Debug, Clone, Default)]
pub struct ProcessSpec {
    /// The executable to run directly, never through a shell.
    pub program: PathBuf,
    /// Arguments forwarded verbatim.
    pub args: Vec<String>,
    /// Optional working directory.
    pub cwd: Option<PathBuf>,
    /// Caller-supplied environment pairs layered over the minimal allowlist.
    pub env: Vec<(String, String)>,
}

/// The result of a completed guarded process.
#[derive(Debug)]
pub struct ProcessOutput {
    /// Exit status of the top-level child.
    pub status: ExitStatus,
    /// Captured standard output.
    pub stdout: Vec<u8>,
    /// Captured standard error.
    pub stderr: Vec<u8>,
    /// Whether observed output exceeded the combined capture.
    pub truncated: bool,
    /// Wall-clock runtime measured from before spawning.
    pub elapsed: Duration,
}

/// Injected timeout policy for probes and downloads.
#[derive(Debug, Clone, Copy)]
pub struct TimeoutPolicy {
    /// Bound for a whole probe run.
    pub probe: Duration,
    /// Bound from spawn to the first progress line of a download.
    pub startup: Duration,
    /// Bound between progress lines once the first line has arrived.
    pub inactivity: Duration,
    /// Bound for terminating and reaping a process tree.
    pub shutdown: Duration,
}

impl Default for TimeoutPolicy {
    fn default() -> Self {
        Self {
            probe: Duration::from_secs(60),
            startup: Duration::from_secs(30),
            inactivity: Duration::from_secs(120),
            shutdown: Duration::from_secs(5),
        }
    }
}

/// Run a probe and return its captured output.
///
/// A timeout kills the whole tree and returns `VIDEO_TIMEOUT`, even when the
/// child was also flooding its pipes. A completed run above the output cap
/// returns `VIDEO_OUTPUT_LIMIT`; an unsuccessful exit returns
/// `VIDEO_PROCESS_FAILED` with a bounded output tail.
pub async fn run_probe(
    spec: &ProcessSpec,
    policy: &TimeoutPolicy,
) -> Result<ProcessOutput, VideoError> {
    let mut guard = spawn_guard(spec)?;
    let capture = Arc::new(Mutex::new(Capture::new()));

    let stdout_task = guard.take_stdout().map(|stdout| {
        let capture = Arc::clone(&capture);
        tokio::spawn(async move {
            read_stream(stdout, capture, StreamKind::Stdout).await;
        })
    });
    let stderr_task = guard.take_stderr().map(|stderr| {
        let capture = Arc::clone(&capture);
        tokio::spawn(async move {
            read_stream(stderr, capture, StreamKind::Stderr).await;
        })
    });

    let started = Instant::now();
    let status = match tokio::time::timeout(policy.probe, guard.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(_)) => {
            let _ = guard.kill_tree().await;
            join_readers(stdout_task, stderr_task).await;
            return Err(VideoError::process_failed(
                "the video process status could not be observed",
            ));
        }
        Err(_) => {
            let _ = guard.kill_tree().await;
            join_readers(stdout_task, stderr_task).await;
            return Err(VideoError::timeout());
        }
    };

    join_readers(stdout_task, stderr_task).await;
    let captured = take_capture(&capture);
    let output = ProcessOutput {
        status,
        stdout: captured.stdout,
        stderr: captured.stderr,
        truncated: captured.truncated,
        elapsed: started.elapsed(),
    };

    if output.truncated {
        return Err(VideoError::output_limit());
    }
    if !output.status.success() {
        return Err(VideoError::process_failed(error_tail(&output)));
    }
    Ok(output)
}

/// Run a download, streaming stdout lines to `on_line`.
///
/// The startup timeout applies until the first complete stdout line arrives and
/// the inactivity timeout applies between later lines. A timeout kills the
/// whole tree and returns `VIDEO_TIMEOUT`; output above the capture cap is
/// truncated without an error, and an unsuccessful exit returns
/// `VIDEO_PROCESS_FAILED`.
pub async fn run_download<F>(
    spec: &ProcessSpec,
    policy: &TimeoutPolicy,
    on_line: F,
) -> Result<ProcessOutput, VideoError>
where
    F: FnMut(&str) + Send + 'static,
{
    let mut guard = spawn_guard(spec)?;
    let capture = Arc::new(Mutex::new(Capture::new()));
    let activity = Arc::new(Activity::new());
    let callback = Arc::new(Mutex::new(on_line));

    let stdout_task = guard.take_stdout().map(|stdout| {
        let capture = Arc::clone(&capture);
        let activity = Arc::clone(&activity);
        let callback = Arc::clone(&callback);
        tokio::spawn(async move {
            read_stdout_lines(stdout, capture, activity, callback).await;
        })
    });
    let stderr_task = guard.take_stderr().map(|stderr| {
        let capture = Arc::clone(&capture);
        tokio::spawn(async move {
            read_stream(stderr, capture, StreamKind::Stderr).await;
        })
    });

    let started = Instant::now();
    let status = loop {
        match guard.try_wait() {
            Ok(Some(status)) => {
                // `wait` also reaps any process group members left behind.
                let _ = guard.wait().await;
                break status;
            }
            Ok(None) => {
                if activity.expired(policy, started) {
                    let _ = guard.kill_tree().await;
                    join_readers(stdout_task, stderr_task).await;
                    return Err(VideoError::timeout());
                }
                tokio::time::sleep(POLL_INTERVAL).await;
            }
            Err(_) => {
                let _ = guard.kill_tree().await;
                join_readers(stdout_task, stderr_task).await;
                return Err(VideoError::process_failed(
                    "the video process status could not be observed",
                ));
            }
        }
    };

    join_readers(stdout_task, stderr_task).await;
    let captured = take_capture(&capture);
    let output = ProcessOutput {
        status,
        stdout: captured.stdout,
        stderr: captured.stderr,
        truncated: captured.truncated,
        elapsed: started.elapsed(),
    };

    if !output.status.success() {
        return Err(VideoError::process_failed(error_tail(&output)));
    }
    Ok(output)
}

/// Why an interruptible download ended.
#[derive(Debug)]
pub enum DownloadRunOutcome {
    /// The process exited successfully and its output was captured.
    Completed(ProcessOutput),
    /// A pause or cancel request killed the whole process tree.
    Cancelled,
}

/// Run a download while a [`DownloadControl`] can kill the whole tree.
///
/// Unlike [`run_download`], the line callback stays on the caller's thread and
/// no reader task is spawned, so the callback can persist progress through a
/// non-`Send` store handle. Cancellation, the startup timeout, and the
/// inactivity timeout all terminate the process group or Job Object before this
/// future resolves.
pub async fn run_download_interruptible<F>(
    spec: &ProcessSpec,
    policy: &TimeoutPolicy,
    control: &DownloadControl,
    mut on_line: F,
) -> Result<DownloadRunOutcome, VideoError>
where
    F: FnMut(&str),
{
    let mut guard = spawn_guard(spec)?;
    let mut stdout = guard.take_stdout();
    let mut stderr = guard.take_stderr();
    let mut capture = Capture::new();
    let mut stdout_buffer = [0u8; READ_CHUNK];
    let mut stderr_buffer = [0u8; READ_CHUNK];
    let mut stdout_carry: Vec<u8> = Vec::new();
    let mut stderr_carry: Vec<u8> = Vec::new();
    let mut stdout_open = stdout.is_some();
    let mut stderr_open = stderr.is_some();
    let mut last_line: Option<Instant> = None;
    let started = Instant::now();
    let mut status: Option<ExitStatus> = None;

    loop {
        if status.is_none() {
            match guard.try_wait() {
                Ok(Some(done)) => {
                    let _ = guard.wait().await;
                    status = Some(done);
                }
                Ok(None) => {}
                Err(_) => {
                    let _ = tokio::time::timeout(policy.shutdown, guard.kill_tree()).await;
                    return Err(VideoError::process_failed(
                        "the video process status could not be observed",
                    ));
                }
            }
        }

        if status.is_some() && !stdout_open && !stderr_open {
            break;
        }

        if control.reason().is_some() {
            let _ = tokio::time::timeout(policy.shutdown, guard.kill_tree()).await;
            return Ok(DownloadRunOutcome::Cancelled);
        }

        let expired = match last_line {
            Some(last) => last.elapsed() >= policy.inactivity,
            None => started.elapsed() >= policy.startup,
        };
        if expired && status.is_none() {
            let _ = tokio::time::timeout(policy.shutdown, guard.kill_tree()).await;
            return Err(VideoError::timeout());
        }

        let stdout_ready = stdout_open;
        let stderr_ready = stderr_open;
        let read_stdout = async {
            if !stdout_ready {
                return std::future::pending().await;
            }
            match stdout.as_mut() {
                Some(reader) => reader.read(&mut stdout_buffer).await,
                None => std::future::pending().await,
            }
        };
        let read_stderr = async {
            if !stderr_ready {
                return std::future::pending().await;
            }
            match stderr.as_mut() {
                Some(reader) => reader.read(&mut stderr_buffer).await,
                None => std::future::pending().await,
            }
        };

        tokio::select! {
            _ = control.notified() => {
                continue;
            }
            result = read_stdout => {
                match result {
                    Ok(0) => {
                        stdout_open = false;
                        if !stdout_carry.is_empty() {
                            let line = String::from_utf8_lossy(&stdout_carry).into_owned();
                            stdout_carry.clear();
                            last_line = Some(Instant::now());
                            on_line(&line);
                        }
                    }
                    Ok(read) => {
                        capture.push(StreamKind::Stdout, &stdout_buffer[..read]);
                        stdout_carry.extend_from_slice(&stdout_buffer[..read]);
                        take_lines(&mut stdout_carry, |line| {
                            last_line = Some(Instant::now());
                            on_line(line);
                        });
                        if stdout_carry.len() > LINE_CARRY_LIMIT {
                            let remove = stdout_carry.len() - LINE_CARRY_LIMIT;
                            stdout_carry.drain(..remove);
                        }
                    }
                    Err(_) => {
                        stdout_open = false;
                    }
                }
            }
            result = read_stderr => {
                match result {
                    Ok(0) => {
                        stderr_open = false;
                        stderr_carry.clear();
                    }
                    Ok(read) => {
                        capture.push(StreamKind::Stderr, &stderr_buffer[..read]);
                        stderr_carry.extend_from_slice(&stderr_buffer[..read]);
                        if stderr_carry.len() > LINE_CARRY_LIMIT {
                            let remove = stderr_carry.len() - LINE_CARRY_LIMIT;
                            stderr_carry.drain(..remove);
                        }
                    }
                    Err(_) => {
                        stderr_open = false;
                    }
                }
            }
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
        }
    }

    let captured = capture.finish();
    let output = ProcessOutput {
        // The loop only breaks once an exit status was observed.
        status: status.expect("the guarded process exited before the loop ended"),
        stdout: captured.stdout,
        stderr: captured.stderr,
        truncated: captured.truncated,
        elapsed: started.elapsed(),
    };

    if control.reason().is_some() {
        return Ok(DownloadRunOutcome::Cancelled);
    }
    if !output.status.success() {
        return Err(VideoError::process_failed(error_tail(&output)));
    }
    Ok(DownloadRunOutcome::Completed(output))
}

fn take_lines<F>(carry: &mut Vec<u8>, mut on_line: F)
where
    F: FnMut(&str),
{
    let mut consumed = 0;
    while let Some(offset) = carry[consumed..].iter().position(|byte| *byte == b'\n') {
        let end = consumed + offset;
        let line = String::from_utf8_lossy(&carry[consumed..end]).into_owned();
        on_line(&line);
        consumed = end + 1;
    }
    if consumed > 0 {
        carry.drain(..consumed);
    }
}

fn spawn_guard(spec: &ProcessSpec) -> Result<ChildGuard, VideoError> {
    let mut command = Command::new(&spec.program);
    command.args(&spec.args);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }

    command.env_clear();
    for key in allowlisted_environment() {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    for (key, value) in &spec.env {
        command.env(key, value);
    }

    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    ChildGuard::spawn(command).map_err(|_| VideoError::spawn_failed())
}

#[cfg(unix)]
fn allowlisted_environment() -> &'static [&'static str] {
    &["PATH", "HOME", "TMPDIR", "TMP", "TEMP"]
}

#[cfg(windows)]
fn allowlisted_environment() -> &'static [&'static str] {
    &["SystemRoot", "USERPROFILE", "PATH", "TEMP", "TMP"]
}

async fn read_stream<R>(mut reader: R, capture: Arc<Mutex<Capture>>, kind: StreamKind)
where
    R: AsyncRead + Unpin,
{
    let mut buffer = [0u8; READ_CHUNK];
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => {
                if let Ok(mut capture) = capture.lock() {
                    capture.push(kind, &buffer[..read]);
                }
            }
            Err(_) => break,
        }
    }
}

async fn read_stdout_lines<F>(
    mut reader: tokio::process::ChildStdout,
    capture: Arc<Mutex<Capture>>,
    activity: Arc<Activity>,
    callback: Arc<Mutex<F>>,
) where
    F: FnMut(&str),
{
    let mut buffer = [0u8; READ_CHUNK];
    let mut carry: Vec<u8> = Vec::new();
    loop {
        match reader.read(&mut buffer).await {
            Ok(0) => break,
            Ok(read) => {
                if let Ok(mut capture) = capture.lock() {
                    capture.push(StreamKind::Stdout, &buffer[..read]);
                }
                carry.extend_from_slice(&buffer[..read]);

                let mut consumed = 0;
                while let Some(offset) = carry[consumed..].iter().position(|byte| *byte == b'\n') {
                    let end = consumed + offset;
                    let line = String::from_utf8_lossy(&carry[consumed..end]).into_owned();
                    deliver_line(&line, &activity, &callback);
                    consumed = end + 1;
                }
                if consumed > 0 {
                    carry.drain(..consumed);
                }
                if carry.len() > LINE_CARRY_LIMIT {
                    let remove = carry.len() - LINE_CARRY_LIMIT;
                    carry.drain(..remove);
                }
            }
            Err(_) => break,
        }
    }

    if !carry.is_empty() {
        let line = String::from_utf8_lossy(&carry).into_owned();
        deliver_line(&line, &activity, &callback);
    }
}

fn deliver_line<F>(line: &str, activity: &Activity, callback: &Mutex<F>)
where
    F: FnMut(&str),
{
    activity.record();
    if let Ok(mut callback) = callback.lock() {
        callback(line);
    }
}

async fn join_readers(
    stdout_task: Option<tokio::task::JoinHandle<()>>,
    stderr_task: Option<tokio::task::JoinHandle<()>>,
) {
    if let Some(task) = stdout_task {
        let _ = task.await;
    }
    if let Some(task) = stderr_task {
        let _ = task.await;
    }
}

fn take_capture(capture: &Mutex<Capture>) -> Captured {
    match capture.lock() {
        Ok(mut capture) => capture.finish(),
        Err(_) => Captured {
            stdout: Vec::new(),
            stderr: Vec::new(),
            truncated: true,
        },
    }
}

fn error_tail(output: &ProcessOutput) -> String {
    let mut text = String::new();
    for bytes in [&output.stderr, &output.stdout] {
        let rendered = String::from_utf8_lossy(bytes);
        let trimmed = rendered.trim();
        if trimmed.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(trimmed);
    }
    let bounded: String = text.chars().rev().take(ERROR_TAIL).collect();
    bounded.chars().rev().collect()
}

#[derive(Clone, Copy)]
enum StreamKind {
    Stdout,
    Stderr,
}

struct Captured {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    truncated: bool,
}

struct Capture {
    remaining: usize,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_tail: VecDeque<u8>,
    stderr_tail: VecDeque<u8>,
    truncated: bool,
}

impl Capture {
    fn new() -> Self {
        Self {
            remaining: COMBINED_OUTPUT_CAP,
            stdout: Vec::new(),
            stderr: Vec::new(),
            stdout_tail: VecDeque::new(),
            stderr_tail: VecDeque::new(),
            truncated: false,
        }
    }

    fn push(&mut self, kind: StreamKind, bytes: &[u8]) {
        let (main, tail) = match kind {
            StreamKind::Stdout => (&mut self.stdout, &mut self.stdout_tail),
            StreamKind::Stderr => (&mut self.stderr, &mut self.stderr_tail),
        };

        let take = bytes.len().min(self.remaining);
        if take > 0 {
            main.extend_from_slice(&bytes[..take]);
            self.remaining -= take;
        }
        if take < bytes.len() {
            self.truncated = true;
        }

        tail.extend(bytes.iter().copied());
        while tail.len() > STREAM_TAIL {
            tail.pop_front();
        }
    }

    fn finish(&mut self) -> Captured {
        if self.truncated {
            self.stdout.extend(self.stdout_tail.drain(..));
            self.stderr.extend(self.stderr_tail.drain(..));
        }
        Captured {
            stdout: std::mem::take(&mut self.stdout),
            stderr: std::mem::take(&mut self.stderr),
            truncated: self.truncated,
        }
    }
}

struct Activity {
    last_line: Mutex<Option<Instant>>,
}

impl Activity {
    fn new() -> Self {
        Self {
            last_line: Mutex::new(None),
        }
    }

    fn record(&self) {
        if let Ok(mut last_line) = self.last_line.lock() {
            *last_line = Some(Instant::now());
        }
    }

    fn expired(&self, policy: &TimeoutPolicy, started: Instant) -> bool {
        match self.last_line.lock() {
            Ok(last_line) => match *last_line {
                Some(last) => last.elapsed() >= policy.inactivity,
                None => started.elapsed() >= policy.startup,
            },
            Err(_) => false,
        }
    }
}
