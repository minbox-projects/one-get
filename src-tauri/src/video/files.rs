//! Task-owned temporary files and final output handling.
//!
//! Output discovery only accepts exactly one non-empty file with the expected
//! extension inside the task temporary directory. Moves and removals go through
//! the destination capability root, and hashing runs on the blocking pool so a
//! large output never stalls the service runtime.

use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::error::VideoError;
use crate::platform::files::CapDir;
use crate::platform::PlatformError;

/// The expected final container extension for a video or audio-only download.
pub(crate) fn output_extension(audio_only: bool) -> &'static str {
    if audio_only {
        "m4a"
    } else {
        "mp4"
    }
}

/// The task temporary directory recorded in the video record, or its default.
pub(crate) fn temp_dir(video: Option<&Value>, destination: &Path, id: &str) -> PathBuf {
    let recorded = video
        .and_then(|record| record.get("tempDir"))
        .and_then(Value::as_str)
        .map(PathBuf::from);
    match recorded {
        Some(path) if path.starts_with(destination) && path != destination => path,
        _ => destination.join(format!(".oneget-video-{id}")),
    }
}

/// Remove a task temporary directory, ignoring a directory that is already gone.
pub(crate) fn remove_temp_dir(path: &Path) {
    if path.exists() {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// Find exactly one non-empty output file with the expected extension.
pub(crate) fn find_output(temp_dir: &Path, extension: &str) -> Result<PathBuf, VideoError> {
    let entries = std::fs::read_dir(temp_dir).map_err(|_| output_missing())?;
    let mut matches = Vec::new();
    for entry in entries {
        let path = entry.map_err(|_| output_missing())?.path();
        if !path.is_file() {
            continue;
        }
        let matches_extension = path
            .extension()
            .and_then(|value| value.to_str())
            .is_some_and(|value| value.eq_ignore_ascii_case(extension));
        if matches_extension {
            matches.push(path);
        }
    }
    if matches.len() != 1 {
        return Err(output_missing());
    }
    let output = matches.remove(0);
    let size = std::fs::metadata(&output)
        .map_err(|_| output_missing())?
        .len();
    if size == 0 {
        return Err(output_missing());
    }
    Ok(output)
}

/// Choose a destination file name that does not collide with an existing entry.
pub(crate) fn unique_name(cap: &CapDir, desired: &str, extension: &str) -> String {
    let safe = if desired.is_empty()
        || desired.contains('/')
        || desired.contains('\\')
        || desired == "."
        || desired == ".."
    {
        format!("video.{extension}")
    } else {
        desired.to_string()
    };

    let path = Path::new(&safe);
    let stem = path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("video");
    let suffix = path
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| value.eq_ignore_ascii_case(extension))
        .map(str::to_string)
        .unwrap_or_else(|| extension.to_string());

    for index in 0..1000 {
        let candidate = if index == 0 {
            format!("{stem}.{suffix}")
        } else {
            format!("{stem} ({index}).{suffix}")
        };
        if !cap.exists(&candidate) {
            return candidate;
        }
    }
    format!("{stem}.{suffix}")
}

/// Move one file from the task temporary directory into the destination root.
pub(crate) fn move_output(
    cap: &CapDir,
    temp_name: &str,
    output_name: &str,
    final_name: &str,
) -> Result<(), VideoError> {
    let source = format!("{temp_name}/{output_name}");
    cap.rename(&source, final_name)
        .map_err(platform_error)
        .map_err(|error| {
            VideoError::new(
                "VIDEO_OUTPUT_MOVE_FAILED",
                format!("the video output could not be moved: {}", error.message()),
            )
        })
}

/// Remove a final file that a cancel won during finalization.
pub(crate) fn remove_final(cap: &CapDir, name: &str) {
    let _ = cap.remove_file(name);
}

/// Stream a file's SHA-256 on the blocking pool.
pub(crate) async fn sha256_file(path: PathBuf) -> Result<String, VideoError> {
    tokio::task::spawn_blocking(move || hash_file(&path))
        .await
        .map_err(|error| {
            VideoError::new(
                "VIDEO_OUTPUT_HASH_FAILED",
                format!("the video output hash task failed: {error}"),
            )
        })?
}

fn hash_file(path: &Path) -> Result<String, VideoError> {
    let mut file = std::fs::File::open(path).map_err(|error| {
        VideoError::new(
            "VIDEO_OUTPUT_MISSING",
            format!("the video output could not be read: {error}"),
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(|error| {
            VideoError::new(
                "VIDEO_OUTPUT_HASH_FAILED",
                format!("the video output hash could not be computed: {error}"),
            )
        })?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn platform_error(error: PlatformError) -> VideoError {
    VideoError::new(error.code, error.message)
}

fn output_missing() -> VideoError {
    VideoError::new(
        "VIDEO_OUTPUT_MISSING",
        "the video download did not produce one non-empty output file",
    )
}
