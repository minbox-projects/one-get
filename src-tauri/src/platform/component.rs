//! Executable preparation for installed component binaries.
//!
//! Preparation validates the path through the capability directory, sets the
//! Unix executable mode, and best-effort removes the macOS quarantine extended
//! attribute. It is idempotent and returns an error for a missing file.

use std::path::Path;

use super::files::CapDir;
use super::PlatformError;

/// Make `path` executable inside `dir`.
///
/// The file must exist and stay inside the capability root. On Unix the mode is
/// set to `0755`; on macOS the `com.apple.quarantine` attribute is removed on a
/// best-effort basis. Repeated calls succeed.
pub fn prepare_executable(dir: &CapDir, path: &str) -> Result<(), PlatformError> {
    let file = dir.open_read(path)?;
    let resolved = std::fs::canonicalize(dir.root().join(path))
        .map_err(|error| PlatformError::io_error(format!("resolve executable {path}"), error))?;
    if !resolved.starts_with(dir.root()) {
        return Err(PlatformError::link_escape(format!(
            "executable escapes capability root: {path}"
        )));
    }

    set_executable_mode(&file, path)?;
    remove_quarantine(&resolved);
    Ok(())
}

#[cfg(unix)]
fn set_executable_mode(file: &std::fs::File, path: &str) -> Result<(), PlatformError> {
    use std::os::unix::fs::PermissionsExt;

    file.set_permissions(std::fs::Permissions::from_mode(0o755))
        .map_err(|error| PlatformError::io_error(format!("set executable mode {path}"), error))
}

#[cfg(not(unix))]
fn set_executable_mode(file: &std::fs::File, path: &str) -> Result<(), PlatformError> {
    let _ = (file, path);
    Ok(())
}

#[cfg(target_os = "macos")]
fn remove_quarantine(path: &Path) {
    use std::process::{Command, Stdio};

    let _ = Command::new("xattr")
        .arg("-d")
        .arg("com.apple.quarantine")
        .arg(path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

#[cfg(not(target_os = "macos"))]
fn remove_quarantine(_path: &Path) {}
