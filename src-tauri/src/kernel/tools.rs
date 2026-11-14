//! Verified resolution of installed component tools.
//!
//! The resolver reads the active records from the component store and verifies
//! each recorded target, version directory, file set, size, and SHA-256 before
//! returning an executable path. Every path is derived from the recorded
//! version directory under the kernel root and read through the capability
//! directory, so a record can never select a path outside that root.

use std::io::Read;
use std::path::PathBuf;

use sha2::{Digest, Sha256};

use super::catalog::Target;
use super::error::KernelError;
use super::store::{ComponentInstall, ComponentStore, InstalledFile};
use crate::platform::files::CapDir;

/// A tool required for a video probe or download.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolRequirement {
    /// The `yt-dlp` executable.
    YtDlp,
    /// The `ffmpeg` executable from the `ffmpeg` component.
    Ffmpeg,
    /// The `ffprobe` executable from the `ffmpeg` component.
    Ffprobe,
    /// The optional `deno` JavaScript runtime.
    Deno,
}

/// Verified paths to the installed component tools.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTools {
    /// The `yt-dlp` executable.
    pub yt_dlp: PathBuf,
    /// The `ffmpeg` executable.
    pub ffmpeg: PathBuf,
    /// The `ffprobe` executable.
    pub ffprobe: PathBuf,
    /// The directory containing both `ffmpeg` and `ffprobe`.
    pub ffmpeg_dir: PathBuf,
    /// The `deno` executable when that component is installed.
    pub deno: Option<PathBuf>,
}

/// Resolves and verifies the installed component tools.
pub struct ToolResolver<'a> {
    store: &'a ComponentStore,
    kernel_root: &'a CapDir,
}

impl<'a> ToolResolver<'a> {
    /// Create a resolver over a component store and kernel root.
    pub fn new(store: &'a ComponentStore, kernel_root: &'a CapDir) -> Self {
        Self { store, kernel_root }
    }

    /// Resolve the required tools and the optional Deno runtime.
    ///
    /// A missing or tampered required component returns `COMPONENT_MISSING` or
    /// `COMPONENT_HASH`; an unsupported record returns `COMPONENT_UNSUPPORTED`
    /// and an inconsistent version directory returns
    /// `COMPONENT_VERSION_MISMATCH`.
    pub fn resolve(&self) -> Result<ResolvedTools, KernelError> {
        let yt_dlp = self.require("yt-dlp", "yt-dlp")?;
        let ffmpeg = self.require("ffmpeg", "ffmpeg")?;
        let ffprobe = self.require("ffmpeg", "ffprobe")?;
        let ffmpeg_dir = ffmpeg
            .parent()
            .map(PathBuf::from)
            .ok_or_else(|| missing("ffmpeg", "ffmpeg"))?;
        let deno = self.optional("deno", "deno")?;

        Ok(ResolvedTools {
            yt_dlp,
            ffmpeg,
            ffprobe,
            ffmpeg_dir,
            deno,
        })
    }

    /// Verify that every requested tool is installed and intact.
    pub fn ensure(&self, requirements: &[ToolRequirement]) -> Result<(), KernelError> {
        for requirement in requirements {
            match requirement {
                ToolRequirement::YtDlp => {
                    self.require("yt-dlp", "yt-dlp")?;
                }
                ToolRequirement::Ffmpeg => {
                    self.require("ffmpeg", "ffmpeg")?;
                }
                ToolRequirement::Ffprobe => {
                    self.require("ffmpeg", "ffprobe")?;
                }
                ToolRequirement::Deno => {
                    self.require("deno", "deno")?;
                }
            }
        }
        Ok(())
    }

    fn require(&self, component: &str, role: &str) -> Result<PathBuf, KernelError> {
        self.optional(component, role)?
            .ok_or_else(|| missing(component, role))
    }

    fn optional(&self, component: &str, role: &str) -> Result<Option<PathBuf>, KernelError> {
        let Some(record) = self.store.active(component)? else {
            return Ok(None);
        };
        verify_record(self.kernel_root, Target::current(), &record)?;
        let file = record
            .files
            .iter()
            .find(|file| file.role == role)
            .ok_or_else(|| missing(component, role))?;
        Ok(Some(
            self.kernel_root
                .root()
                .join(&record.version_dir)
                .join(&file.name),
        ))
    }
}

/// Verify a recorded installation against the current target and the bytes on
/// disk, returning the recorded version directory name.
///
/// The target, version directory, file set, per-file size, and per-file SHA-256
/// are checked; the file bytes are read through the capability directory so an
/// escape outside the kernel root is refused.
pub(crate) fn verify_record(
    kernel_root: &CapDir,
    target: Target,
    record: &ComponentInstall,
) -> Result<(), KernelError> {
    if record.target != target.as_str() {
        return Err(KernelError::new(
            "COMPONENT_UNSUPPORTED",
            format!(
                "component `{}` is recorded for another target",
                record.component
            ),
        ));
    }

    let expected_dir = version_directory(&record.component, record.sequence, &record.version);
    if record.version_dir != expected_dir {
        return Err(KernelError::new(
            "COMPONENT_VERSION_MISMATCH",
            format!(
                "component `{}` has an inconsistent version directory",
                record.component
            ),
        ));
    }

    for role in expected_roles(&record.component) {
        if !record.files.iter().any(|file| file.role == *role) {
            return Err(missing(&record.component, role));
        }
    }

    let directory = kernel_root.root().join(&record.version_dir);
    let mut found: Vec<String> = std::fs::read_dir(&directory)
        .map_err(|error| io_read_error(&record.component, &record.version_dir, error))?
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            entry
                .file_type()
                .ok()
                .filter(|file_type| file_type.is_file())
                .map(|_| entry.file_name().to_string_lossy().into_owned())
        })
        .collect();
    for file in &record.files {
        if !found.iter().any(|name| name == &file.name) {
            return Err(missing(&record.component, &file.role));
        }
    }
    let mut expected: Vec<String> = record.files.iter().map(|file| file.name.clone()).collect();
    expected.sort_unstable();
    found.sort_unstable();
    if found != expected {
        return Err(KernelError::new(
            "COMPONENT_HASH",
            format!(
                "component `{}` file set does not match its record",
                record.component
            ),
        ));
    }

    for file in &record.files {
        let relative = format!("{}/{}", record.version_dir, file.name);
        let data = read_capability(kernel_root, &relative, &record.component, file)?;
        if data.len() as u64 != file.size || hex::encode(Sha256::digest(&data)) != file.sha256 {
            return Err(KernelError::new(
                "COMPONENT_HASH",
                format!(
                    "component `{}` file `{}` does not match its recorded hash or size",
                    record.component, file.name
                ),
            ));
        }
    }

    Ok(())
}

fn read_capability(
    kernel_root: &CapDir,
    relative: &str,
    component: &str,
    file: &InstalledFile,
) -> Result<Vec<u8>, KernelError> {
    let mut handle = kernel_root
        .open_read(relative)
        .map_err(|error| read_error(component, &file.name, error))?;
    let mut data = Vec::new();
    handle
        .read_to_end(&mut data)
        .map_err(|error| io_read_error(component, &file.name, error))?;
    Ok(data)
}

fn read_error(component: &str, name: &str, error: crate::platform::PlatformError) -> KernelError {
    if error.code == "not_found" {
        missing(component, name)
    } else {
        KernelError::new(
            "COMPONENT_HASH",
            format!("component `{component}` file `{name}` could not be read: {error}"),
        )
    }
}

fn io_read_error(component: &str, name: &str, error: std::io::Error) -> KernelError {
    if error.kind() == std::io::ErrorKind::NotFound {
        missing(component, name)
    } else {
        KernelError::new(
            "COMPONENT_HASH",
            format!("component `{component}` file `{name}` could not be read: {error}"),
        )
    }
}

fn expected_roles(component: &str) -> &'static [&'static str] {
    match component {
        "yt-dlp" => &["yt-dlp"],
        "ffmpeg" => &["ffmpeg", "ffprobe"],
        "deno" => &["deno"],
        _ => &[],
    }
}

fn missing(component: &str, role: &str) -> KernelError {
    KernelError::new(
        "COMPONENT_MISSING",
        format!("component `{component}` is missing its `{role}` file"),
    )
}

fn version_directory(component: &str, sequence: u64, display_version: &str) -> String {
    format!(
        "{component}/{sequence}-{}",
        sanitize_display_version(display_version)
    )
}

fn sanitize_display_version(version: &str) -> String {
    let mut sanitized = String::with_capacity(version.len());
    for character in version.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
            sanitized.push(character);
        } else {
            sanitized.push('-');
        }
    }
    let trimmed = sanitized.trim_matches(|character| character == '-' || character == '.');
    if trimmed.is_empty() {
        "version".to_string()
    } else {
        trimmed.to_string()
    }
}
