//! Execution of one claimed video task.
//!
//! The service resolves an absolute yt-dlp allowlist, runs it inside the
//! task-owned temporary directory, persists throttled progress, then moves the
//! single expected output through the destination capability root and records
//! its SHA-256 before completing the task.

use std::rc::Rc;
use std::time::Instant;

use serde_json::json;

use super::control::{ControlAction, DownloadControl};
use super::files;
use super::probe::proxy_environment;
use super::process::{run_download_interruptible, DownloadRunOutcome, ProcessSpec};
use super::progress::{is_merge_line, parse_progress_line};
use super::service::{
    fail_video_task, read_video_flag, read_video_string, resolve_execution_tools, resolve_full_url,
    task_destination, transition_to, ExecutionTools, VideoRuntime,
};
use crate::engine::state::TaskState;
use crate::platform::files::CapDir;
use crate::store::Task;

/// The result the service actor persists after one run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    /// The final file exists, its hash is stored, and the task is completed.
    Completed,
    /// The task is already failed with a stable error code.
    Failed,
    /// A pause or cancel stopped the run before completion.
    Cancelled,
}

/// Run one claimed video task to completion, failure, or interruption.
pub(crate) async fn run_video_task(
    runtime: Rc<VideoRuntime>,
    task: Task,
    control: DownloadControl,
) -> RunOutcome {
    let id = task.id.clone();
    let destination = task_destination(&runtime, &task);
    let audio_only = read_video_flag(&task, "audioOnly");
    let extension = files::output_extension(audio_only);
    let temp_name = format!(".oneget-video-{id}");
    let temp_path = destination.join(&temp_name);

    let full_url = match resolve_full_url(&runtime, &task) {
        Ok(url) => url,
        Err(error) => {
            return fail_video_task(&runtime, &id, error.code(), error.message());
        }
    };
    let tools = match resolve_execution_tools(&runtime.config) {
        Ok(tools) => tools,
        Err(error) => {
            return fail_video_task(&runtime, &id, error.code(), error.message());
        }
    };

    let cap = match CapDir::open(&destination) {
        Ok(cap) => cap,
        Err(error) => {
            return fail_video_task(&runtime, &id, error.code, &error.message);
        }
    };
    let reuse = temp_path.exists();
    if let Err(error) = cap.create_dir_all(&temp_name) {
        return fail_video_task(&runtime, &id, error.code, &error.message);
    }
    if let Err(error) = runtime
        .config
        .store
        .patch_video_json(&id, &json!({ "tempDir": temp_path.to_string_lossy() }))
    {
        return fail_video_task(&runtime, &id, error.code(), &error.to_string());
    }

    let spec = ProcessSpec {
        program: tools.yt_dlp.clone(),
        args: download_args(&task, &tools, &full_url, &temp_path, audio_only, reuse),
        cwd: None,
        env: proxy_environment(
            runtime.proxy().as_ref(),
            read_video_string(&task, "proxyUrl").as_deref(),
        ),
    };

    let store = &runtime.config.store;
    let interval = runtime.config.progress_persist_interval;
    let mut last_persisted = Instant::now()
        .checked_sub(interval)
        .unwrap_or_else(Instant::now);
    let mut latest: Option<super::progress::ProgressUpdate> = None;
    let mut merge_seen = false;
    let mut downloading = false;
    let trace_id = id.clone();
    let progress_runtime = Rc::clone(&runtime);

    let on_line = |line: &str| {
        if !downloading {
            downloading = true;
            let _ = transition_to(&progress_runtime, &trace_id, TaskState::Downloading);
        }
        if !merge_seen && is_merge_line(line) {
            merge_seen = true;
            let _ = store.patch_video_json(&trace_id, &json!({ "mergeStage": "merge" }));
        }
        if let Some(update) = parse_progress_line(line) {
            latest = Some(update);
            let now = Instant::now();
            if now.duration_since(last_persisted) >= interval {
                let _ = store.persist_video_progress(
                    &trace_id,
                    update.downloaded_bytes,
                    update.total_bytes,
                );
                last_persisted = now;
            }
        }
    };

    let outcome =
        run_download_interruptible(&spec, &runtime.config.timeouts, &control, on_line).await;

    let final_update = latest;
    match outcome {
        // A downloader that cannot find a JavaScript runtime reports that in
        // its output; surface it as its own typed failure.
        Err(error) if error.is_missing_js_runtime() => fail_video_task(
            &runtime,
            &id,
            "VIDEO_JS_RUNTIME_MISSING",
            "a supported JavaScript runtime is required but none is available",
        ),
        // Once the merge stage started, an unsuccessful process exit means the
        // streams could not be combined. The temporary directory is preserved
        // and no final file is created because finalization never runs.
        Err(error) if merge_seen && error.code() == "VIDEO_PROCESS_FAILED" => fail_video_task(
            &runtime,
            &id,
            "VIDEO_MERGE_FAILED",
            "the video and audio streams could not be merged",
        ),
        Err(error) => fail_video_task(&runtime, &id, error.code(), error.message()),
        Ok(DownloadRunOutcome::Cancelled) => RunOutcome::Cancelled,
        Ok(DownloadRunOutcome::Completed(_)) => {
            if let Some(update) = final_update {
                let _ =
                    store.persist_video_progress(&id, update.downloaded_bytes, update.total_bytes);
            }
            finalize(
                &runtime,
                &id,
                &task,
                &cap,
                &destination,
                &temp_name,
                &temp_path,
                extension,
                &control,
            )
            .await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn finalize(
    runtime: &Rc<VideoRuntime>,
    id: &str,
    task: &Task,
    cap: &CapDir,
    destination: &std::path::Path,
    temp_name: &str,
    temp_path: &std::path::Path,
    extension: &str,
    control: &DownloadControl,
) -> RunOutcome {
    // A very fast or silent process may complete before its first progress
    // line, so move through the downloading state before finalizing.
    if let Err(error) = transition_to(runtime, id, TaskState::Downloading) {
        return fail_video_task(runtime, id, error.code(), error.message());
    }
    if let Err(error) = transition_to(runtime, id, TaskState::Finalizing) {
        return fail_video_task(runtime, id, error.code(), error.message());
    }

    let output = match files::find_output(temp_path, extension) {
        Ok(output) => output,
        Err(error) => {
            return fail_video_task(runtime, id, error.code(), error.message());
        }
    };
    let output_name = output
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("video")
        .to_string();
    let final_name = files::unique_name(cap, &task.filename, extension);
    if let Err(error) = files::move_output(cap, temp_name, &output_name, &final_name) {
        return fail_video_task(runtime, id, error.code(), error.message());
    }

    if matches!(control.reason(), Some(ControlAction::Cancel)) {
        files::remove_final(cap, &final_name);
        files::remove_temp_dir(temp_path);
        return RunOutcome::Cancelled;
    }

    let final_path = destination.join(&final_name);
    let hash = match files::sha256_file(final_path.clone()).await {
        Ok(hash) => hash,
        Err(error) => {
            files::remove_final(cap, &final_name);
            return fail_video_task(runtime, id, error.code(), error.message());
        }
    };

    if matches!(control.reason(), Some(ControlAction::Cancel)) {
        files::remove_final(cap, &final_name);
        files::remove_temp_dir(temp_path);
        return RunOutcome::Cancelled;
    }

    let final_path_value = final_path.to_string_lossy().into_owned();
    match runtime
        .config
        .store
        .complete_video_task(id, &final_name, &final_path_value, &hash)
    {
        Ok(()) => {
            files::remove_temp_dir(temp_path);
            RunOutcome::Completed
        }
        Err(error) => {
            let code = error.code().to_string();
            fail_video_task(runtime, id, &code, &error.to_string())
        }
    }
}

fn download_args(
    task: &Task,
    tools: &ExecutionTools,
    full_url: &str,
    temp_path: &std::path::Path,
    audio_only: bool,
    reuse: bool,
) -> Vec<String> {
    let selector = read_video_string(task, "formatSelector")
        .unwrap_or_else(|| "bestvideo+bestaudio/best".to_string());
    let output_template = format!("{}/video.%(ext)s", temp_path.display());

    let mut args = vec![
        "--newline".to_string(),
        "--no-playlist".to_string(),
        "--no-warnings".to_string(),
        "--ignore-config".to_string(),
        "--progress-template".to_string(),
        "download:ONEGET_PROGRESS %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.speed)s".to_string(),
        "-f".to_string(),
        selector,
        "--concurrent-fragments".to_string(),
        "4".to_string(),
        "--ffmpeg-location".to_string(),
        tools.ffmpeg_dir.to_string_lossy().into_owned(),
        "-o".to_string(),
        output_template,
    ];

    if reuse {
        args.push("--continue".to_string());
    }
    if let Some(cookies_file) = read_video_string(task, "cookiesFile") {
        args.push("--cookies".to_string());
        args.push(cookies_file);
    }
    if let Some(proxy_url) = read_video_string(task, "proxyUrl") {
        args.push("--proxy".to_string());
        args.push(proxy_url);
    }
    if let Some(deno) = &tools.deno {
        args.push("--js-runtimes".to_string());
        args.push(format!("deno:{}", deno.display()));
    }

    if audio_only {
        args.push("--extract-audio".to_string());
        args.push("--audio-format".to_string());
        args.push("m4a".to_string());
    } else {
        args.push("--merge-output-format".to_string());
        args.push("mp4".to_string());
    }

    args.push(full_url.to_string());
    args
}
