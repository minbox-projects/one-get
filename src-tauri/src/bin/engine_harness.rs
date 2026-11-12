//! Minimal command-line harness for the download engine.
//!
//! It supports running a task to completion against a local fixture, inspecting
//! a task's persisted state, and applying pause, resume, or cancel controls.
//! Output is bounded and never includes URLs, cookies, proxy credentials, or
//! request header values.

use std::path::{Path, PathBuf};
use std::time::Duration;

use oneget_lib::engine::scheduler::{Engine, EngineOptions};
use oneget_lib::engine::state::TaskState;
use oneget_lib::engine::worker::DownloadSpec;
use oneget_lib::store::{Database, Task};

const POLL_INTERVAL: Duration = Duration::from_millis(10);

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("run") => run_command(&args[1..]).await,
        Some("status") => status_command(&args[1..]),
        Some("pause") | Some("resume") | Some("cancel") => {
            control_command(args[0].as_str(), &args[1..]).await
        }
        Some(other) => Err(format!("unknown command: {other}")),
        None => Err(String::from(
            "usage: engine_harness run|status|pause|resume|cancel [options]",
        )),
    };

    if let Err(error) = result {
        eprintln!("engine_harness: {error}");
        std::process::exit(1);
    }
}

async fn run_command(args: &[String]) -> Result<(), String> {
    let db_path = required(args, "--db")?;
    let dest = PathBuf::from(required(args, "--dest")?);
    let url = required(args, "--url")?;
    let filename = required(args, "--filename")?;
    let connections = optional(args, "--connections")
        .map(|value| value.parse::<u32>())
        .transpose()
        .map_err(|_| String::from("--connections must be a positive integer"))?
        .unwrap_or(1);
    let expected_sha256 = optional(args, "--expected-sha256");

    let store = Database::open(&db_path).map_err(|error| error.to_string())?;
    let existing = store
        .list_tasks()
        .map_err(|error| error.to_string())?
        .into_iter()
        .find(|task| task.url == url && task.filename == filename);

    let engine = Engine::new(
        Database::open(&db_path).map_err(|error| error.to_string())?,
        dest.clone(),
        EngineOptions { max_concurrent: 4 },
    );

    let task_id = match existing {
        Some(task) => {
            let id = task.id.clone();
            if let Some(expected) = expected_sha256.as_deref() {
                store
                    .record_expected_sha256(&id, expected)
                    .map_err(|error| error.to_string())?;
            }
            match task.state {
                TaskState::Failed => engine.retry(&id).await.map_err(|error| error.to_string())?,
                TaskState::Paused => engine
                    .resume(&id)
                    .await
                    .map_err(|error| error.to_string())?,
                _ => {}
            }
            id
        }
        None => engine
            .add_task(DownloadSpec {
                url,
                filename: filename.clone(),
                connections,
                allow_html: false,
                expected_sha256,
                extra_headers: Vec::new(),
            })
            .map_err(|error| error.to_string())?,
    };

    let monitor = Database::open(&db_path).map_err(|error| error.to_string())?;
    let mut ticker = tokio::time::interval(POLL_INTERVAL);
    let mut last_downloaded = u64::MAX;
    let run = engine.run();
    tokio::pin!(run);

    loop {
        tokio::select! {
            result = &mut run => {
                result.map_err(|error| error.to_string())?;
                break;
            }
            _ = ticker.tick() => {
                if let Ok(Some(task)) = monitor.task(&task_id) {
                    if task.downloaded_bytes != last_downloaded {
                        println!("progress {}", task.downloaded_bytes);
                        last_downloaded = task.downloaded_bytes;
                    }
                }
            }
        }
    }

    let task = load_task(&monitor, &task_id)?;
    match task.state {
        TaskState::Completed => {
            println!(
                "completed {} {}",
                task.filename,
                task.actual_sha256.unwrap_or_default()
            );
            Ok(())
        }
        other => Err(format!(
            "task ended in state {}: {}",
            other.as_str(),
            task.error_message.unwrap_or_default()
        )),
    }
}

fn status_command(args: &[String]) -> Result<(), String> {
    let db_path = required(args, "--db")?;
    let id = required(args, "--id")?;
    let store = Database::open(&db_path).map_err(|error| error.to_string())?;
    let task = load_task(&store, &id)?;
    println!(
        "state {} downloaded {}",
        task.state.as_str(),
        task.downloaded_bytes
    );
    Ok(())
}

async fn control_command(command: &str, args: &[String]) -> Result<(), String> {
    let db_path = required(args, "--db")?;
    let id = required(args, "--id")?;
    let dest = optional(args, "--dest")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            Path::new(&db_path)
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default()
        });

    let engine = Engine::new(
        Database::open(&db_path).map_err(|error| error.to_string())?,
        dest,
        EngineOptions { max_concurrent: 4 },
    );

    match command {
        "pause" => engine.pause(&id).await,
        "resume" => engine.resume(&id).await,
        "cancel" => engine.cancel(&id).await,
        _ => unreachable!("validated command"),
    }
    .map_err(|error| error.to_string())
}

fn load_task(store: &Database, id: &str) -> Result<Task, String> {
    store
        .task(id)
        .map_err(|error| error.to_string())?
        .ok_or_else(|| format!("task {id} does not exist"))
}

fn required(args: &[String], flag: &str) -> Result<String, String> {
    optional(args, flag).ok_or_else(|| format!("missing required {flag}"))
}

fn optional(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|argument| argument == flag)
        .and_then(|index| args.get(index + 1))
        .cloned()
}
