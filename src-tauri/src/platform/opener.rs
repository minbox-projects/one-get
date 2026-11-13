//! Shell-free opening of a file or folder with the platform's default handler.

use std::path::Path;
use std::process::{Command, Stdio};

use super::PlatformError;

/// Open `path` with the operating system's default handler.
///
/// The target must already exist. The handler is spawned directly, never
/// through a shell, so the path cannot be reinterpreted as shell syntax.
pub fn open_target(path: &Path) -> Result<(), PlatformError> {
    if !path.exists() {
        return Err(PlatformError::not_found("the target does not exist"));
    }

    let mut command = platform_command(path);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.spawn().map_err(|error| {
        PlatformError::new(
            "spawn_failed",
            format!("the target could not be opened: {error}"),
        )
    })?;
    Ok(())
}

#[cfg(target_os = "macos")]
fn platform_command(path: &Path) -> Command {
    let mut command = Command::new("open");
    command.arg(path);
    command
}

#[cfg(windows)]
fn platform_command(path: &Path) -> Command {
    let mut command = Command::new("explorer.exe");
    command.arg(path);
    command
}

#[cfg(not(any(target_os = "macos", windows)))]
fn platform_command(path: &Path) -> Command {
    let mut command = Command::new("xdg-open");
    command.arg(path);
    command
}
