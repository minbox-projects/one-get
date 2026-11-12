//! Capability-scoped asynchronous file operations.
//!
//! Every operation is confined to a canonical root directory. Paths are
//! validated as relative, separator-free, parent-free components; existing
//! targets are resolved and rejected when they escape the root, and writes or
//! creates refuse a final symbolic link.

use std::fs::File;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use cap_std::ambient_authority;
use cap_std::fs::{Dir, OpenOptions};

use super::PlatformError;

/// Basic metadata for a file inside a [`CapDir`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMetadata {
    /// File size in bytes.
    pub len: u64,
    /// Whether the entry is a regular file.
    pub is_file: bool,
}

/// A filesystem capability rooted at a canonical directory.
#[derive(Clone)]
pub struct CapDir {
    inner: Arc<CapInner>,
}

struct CapInner {
    dir: Dir,
    root: PathBuf,
}

impl CapDir {
    /// Open (creating if needed) and canonicalize `root` as a capability.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, PlatformError> {
        let root = root.as_ref();
        std::fs::create_dir_all(root).map_err(|error| {
            PlatformError::io_error(format!("create capability root {}", root.display()), error)
        })?;
        let root = std::fs::canonicalize(root).map_err(|error| {
            PlatformError::io_error(
                format!("canonicalize capability root {}", root.display()),
                error,
            )
        })?;
        let dir = Dir::open_ambient_dir(&root, ambient_authority()).map_err(|error| {
            PlatformError::io_error(format!("open capability root {}", root.display()), error)
        })?;
        Ok(Self {
            inner: Arc::new(CapInner { dir, root }),
        })
    }

    /// Canonical root directory.
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    /// Create a new file, failing if it already exists.
    pub async fn create(&self, path: &str) -> Result<(), PlatformError> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || inner.create(&path))
            .await
            .map_err(join_error)?
    }

    /// Write `contents`, creating or truncating the file.
    pub async fn write(&self, path: &str, contents: &[u8]) -> Result<(), PlatformError> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        let contents = contents.to_vec();
        tokio::task::spawn_blocking(move || inner.write(&path, &contents))
            .await
            .map_err(join_error)?
    }

    /// Read the entire file.
    pub async fn read(&self, path: &str) -> Result<Vec<u8>, PlatformError> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || inner.read(&path))
            .await
            .map_err(join_error)?
    }

    /// Flush a file's contents to durable storage.
    pub async fn sync(&self, path: &str) -> Result<(), PlatformError> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || inner.sync(&path))
            .await
            .map_err(join_error)?
    }

    /// Read metadata for a file.
    pub async fn metadata(&self, path: &str) -> Result<FileMetadata, PlatformError> {
        let inner = Arc::clone(&self.inner);
        let path = path.to_owned();
        tokio::task::spawn_blocking(move || inner.metadata(&path))
            .await
            .map_err(join_error)?
    }

    /// Rename `source` over `destination`, replacing an existing destination.
    pub async fn replace(&self, source: &str, destination: &str) -> Result<(), PlatformError> {
        let inner = Arc::clone(&self.inner);
        let source = source.to_owned();
        let destination = destination.to_owned();
        tokio::task::spawn_blocking(move || inner.replace(&source, &destination))
            .await
            .map_err(join_error)?
    }

    /// Open an existing file for reading through the capability.
    pub fn open_read(&self, path: &str) -> Result<File, PlatformError> {
        self.inner.open_read(path)
    }

    /// Open an existing file for reading and writing through the capability.
    pub fn open_rw(&self, path: &str) -> Result<File, PlatformError> {
        self.inner.open_rw(path)
    }

    /// Open a file for reading and writing, creating it when it is absent.
    pub fn open_rw_create(&self, path: &str) -> Result<File, PlatformError> {
        self.inner.open_rw_create(path)
    }

    /// Extend or shrink a file to `len` bytes through the capability.
    pub fn preallocate(&self, path: &str, len: u64) -> Result<(), PlatformError> {
        self.inner.preallocate(path, len)
    }

    /// Flush the capability root directory to durable storage.
    ///
    /// On Unix this fsyncs the root directory. On Windows it is a best-effort
    /// no-op because the platform does not expose a directory fsync.
    pub fn sync_dir(&self) -> Result<(), PlatformError> {
        self.inner.sync_dir()
    }

    /// Rename `source` over `destination` from synchronous code.
    pub fn replace_blocking(&self, source: &str, destination: &str) -> Result<(), PlatformError> {
        self.inner.replace(source, destination)
    }
}

fn join_error(error: tokio::task::JoinError) -> PlatformError {
    PlatformError::new("io_error", format!("blocking file task failed: {error}"))
}

impl CapInner {
    fn create(&self, raw: &str) -> Result<(), PlatformError> {
        let relative = self.validate(raw)?;
        self.prepare_write(&relative)?;
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        let _file = self
            .dir
            .open_with(relative.as_path(), &options)
            .map_err(|error| map_io("create", raw, error))?;
        Ok(())
    }

    fn write(&self, raw: &str, contents: &[u8]) -> Result<(), PlatformError> {
        let relative = self.validate(raw)?;
        self.prepare_write(&relative)?;
        self.dir
            .write(relative.as_path(), contents)
            .map_err(|error| map_io("write", raw, error))
    }

    fn open_read(&self, raw: &str) -> Result<File, PlatformError> {
        let relative = self.validate(raw)?;
        let resolved = self.resolve_existing(&relative, raw)?;
        let metadata =
            std::fs::metadata(&resolved).map_err(|error| map_io("open metadata", raw, error))?;
        if metadata.is_dir() {
            return Err(PlatformError::invalid_path(format!(
                "path is a directory: {raw}"
            )));
        }
        let file = self
            .dir
            .open(relative.as_path())
            .map_err(|error| map_io("open", raw, error))?;
        Ok(file.into_std())
    }

    fn open_rw(&self, raw: &str) -> Result<File, PlatformError> {
        let relative = self.validate(raw)?;
        self.prepare_write(&relative)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        let file = self
            .dir
            .open_with(relative.as_path(), &options)
            .map_err(|error| map_io("open", raw, error))?;
        Ok(file.into_std())
    }

    fn open_rw_create(&self, raw: &str) -> Result<File, PlatformError> {
        let relative = self.validate(raw)?;
        self.prepare_write(&relative)?;
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        let file = self
            .dir
            .open_with(relative.as_path(), &options)
            .map_err(|error| map_io("open", raw, error))?;
        Ok(file.into_std())
    }

    fn preallocate(&self, raw: &str, len: u64) -> Result<(), PlatformError> {
        let relative = self.validate(raw)?;
        self.prepare_write(&relative)?;
        let mut options = OpenOptions::new();
        options.write(true).create(true);
        let file = self
            .dir
            .open_with(relative.as_path(), &options)
            .map_err(|error| map_io("preallocate", raw, error))?;
        file.set_len(len)
            .map_err(|error| map_io("preallocate", raw, error))
    }

    #[cfg(unix)]
    fn sync_dir(&self) -> Result<(), PlatformError> {
        let directory = std::fs::File::open(&self.root)
            .map_err(|error| PlatformError::io_error("open capability root for sync", error))?;
        directory
            .sync_all()
            .map_err(|error| PlatformError::io_error("sync capability root", error))
    }

    #[cfg(not(unix))]
    fn sync_dir(&self) -> Result<(), PlatformError> {
        Ok(())
    }

    fn read(&self, raw: &str) -> Result<Vec<u8>, PlatformError> {
        let relative = self.validate(raw)?;
        let resolved = self.resolve_existing(&relative, raw)?;
        let metadata =
            std::fs::metadata(&resolved).map_err(|error| map_io("read metadata", raw, error))?;
        if metadata.is_dir() {
            return Err(PlatformError::invalid_path(format!(
                "path is a directory: {raw}"
            )));
        }
        self.dir
            .read(relative.as_path())
            .map_err(|error| map_io("read", raw, error))
    }

    fn sync(&self, raw: &str) -> Result<(), PlatformError> {
        let relative = self.validate(raw)?;
        self.resolve_existing(&relative, raw)?;
        let mut options = OpenOptions::new();
        options.write(true);
        let file = self
            .dir
            .open_with(relative.as_path(), &options)
            .map_err(|error| map_io("open for sync", raw, error))?;
        file.sync_all().map_err(|error| map_io("sync", raw, error))
    }

    fn metadata(&self, raw: &str) -> Result<FileMetadata, PlatformError> {
        let relative = self.validate(raw)?;
        let resolved = self.resolve_existing(&relative, raw)?;
        let metadata =
            std::fs::metadata(&resolved).map_err(|error| map_io("metadata", raw, error))?;
        Ok(FileMetadata {
            len: metadata.len(),
            is_file: metadata.is_file(),
        })
    }

    fn replace(&self, source: &str, destination: &str) -> Result<(), PlatformError> {
        let source_rel = self.validate(source)?;
        let destination_rel = self.validate(destination)?;

        let source_path = self.root.join(&source_rel);
        let source_metadata = std::fs::symlink_metadata(&source_path)
            .map_err(|error| map_io("inspect replace source", source, error))?;
        if source_metadata.file_type().is_symlink() {
            return Err(PlatformError::link_escape(format!(
                "refusing to rename symbolic link source: {source}"
            )));
        }
        if source_metadata.is_dir() {
            return Err(PlatformError::invalid_path(format!(
                "replace source is a directory: {source}"
            )));
        }
        self.resolve_existing(&source_rel, source)?;

        let destination_path = self.root.join(&destination_rel);
        match std::fs::symlink_metadata(&destination_path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(PlatformError::link_escape(format!(
                        "refusing to replace symbolic link destination: {destination}"
                    )));
                }
                if metadata.is_dir() {
                    return Err(PlatformError::invalid_path(format!(
                        "replace destination is a directory: {destination}"
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(PlatformError::io_error(
                    format!("inspect replace destination {destination}"),
                    error,
                ));
            }
        }

        self.ensure_parent(&source_rel)?;
        self.ensure_parent(&destination_rel)?;

        if final_is_symlink(&source_path) {
            return Err(PlatformError::link_escape(format!(
                "refusing to rename symbolic link source: {source}"
            )));
        }
        if final_is_symlink(&destination_path) {
            return Err(PlatformError::link_escape(format!(
                "refusing to replace symbolic link destination: {destination}"
            )));
        }

        self.dir
            .rename(source_rel.as_path(), &self.dir, destination_rel.as_path())
            .map_err(|error| {
                PlatformError::io_error(format!("replace {source} with {destination}"), error)
            })
    }

    fn validate(&self, raw: &str) -> Result<PathBuf, PlatformError> {
        validate_relative(raw)
    }

    fn resolve_existing(&self, relative: &Path, raw: &str) -> Result<PathBuf, PlatformError> {
        let candidate = self.root.join(relative);
        let resolved =
            std::fs::canonicalize(&candidate).map_err(|error| map_io("resolve", raw, error))?;
        if !resolved.starts_with(&self.root) {
            return Err(PlatformError::link_escape(format!(
                "path escapes capability root: {raw}"
            )));
        }
        Ok(resolved)
    }

    fn prepare_write(&self, relative: &Path) -> Result<(), PlatformError> {
        let candidate = self.root.join(relative);
        match std::fs::symlink_metadata(&candidate) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() {
                    return Err(PlatformError::link_escape(format!(
                        "refusing to write through symbolic link: {}",
                        relative.display()
                    )));
                }
                if metadata.is_dir() {
                    return Err(PlatformError::invalid_path(format!(
                        "path is a directory: {}",
                        relative.display()
                    )));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(PlatformError::io_error(
                    format!("inspect write target {}", candidate.display()),
                    error,
                ));
            }
        }
        self.ensure_parent(relative)
    }

    fn ensure_parent(&self, relative: &Path) -> Result<(), PlatformError> {
        let parent_relative = relative.parent().unwrap_or(Path::new(""));
        if parent_relative.as_os_str().is_empty() {
            return Ok(());
        }

        let parent_absolute = self.root.join(parent_relative);
        let mut existing = parent_absolute.as_path();
        loop {
            match std::fs::symlink_metadata(existing) {
                Ok(metadata) => {
                    if !metadata.is_dir() && !metadata.file_type().is_symlink() {
                        return Err(PlatformError::invalid_path(format!(
                            "parent component is not a directory: {}",
                            existing.display()
                        )));
                    }
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    existing = existing.parent().ok_or_else(|| {
                        PlatformError::invalid_path(format!(
                            "path has no existing ancestor: {}",
                            relative.display()
                        ))
                    })?;
                }
                Err(error) => {
                    return Err(PlatformError::io_error(
                        format!("inspect parent {}", existing.display()),
                        error,
                    ));
                }
            }
        }

        let resolved = std::fs::canonicalize(existing).map_err(|error| {
            PlatformError::io_error(format!("canonicalize parent {}", existing.display()), error)
        })?;
        if !resolved.starts_with(&self.root) {
            return Err(PlatformError::link_escape(format!(
                "parent escapes capability root: {}",
                existing.display()
            )));
        }

        self.dir.create_dir_all(parent_relative).map_err(|error| {
            PlatformError::io_error(
                format!("create parent {}", parent_absolute.display()),
                error,
            )
        })?;

        let created = std::fs::canonicalize(&parent_absolute).map_err(|error| {
            PlatformError::io_error(
                format!("canonicalize parent {}", parent_absolute.display()),
                error,
            )
        })?;
        if !created.starts_with(&self.root) {
            return Err(PlatformError::link_escape(format!(
                "parent escapes capability root: {}",
                parent_absolute.display()
            )));
        }
        Ok(())
    }
}

/// Write `bytes` at an absolute file offset without changing the cursor.
///
/// The platform-specific branch stays in this module: Unix uses `write_all_at`
/// and Windows loops over `seek_write`.
#[cfg(unix)]
pub fn write_at(file: &File, offset: u64, bytes: &[u8]) -> Result<(), PlatformError> {
    use std::os::unix::fs::FileExt;

    file.write_all_at(bytes, offset)
        .map_err(|error| PlatformError::io_error("write at offset", error))
}

/// Write `bytes` at an absolute file offset without changing the cursor.
#[cfg(windows)]
pub fn write_at(file: &File, offset: u64, bytes: &[u8]) -> Result<(), PlatformError> {
    use std::io::ErrorKind;
    use std::os::windows::fs::FileExt;

    let mut written = 0usize;
    while written < bytes.len() {
        let position = offset + written as u64;
        let count = file
            .seek_write(&bytes[written..], position)
            .map_err(|error| PlatformError::io_error("write at offset", error))?;
        if count == 0 {
            return Err(PlatformError::io_error(
                "write at offset",
                std::io::Error::new(ErrorKind::WriteZero, "the file accepted zero bytes"),
            ));
        }
        written += count;
    }
    Ok(())
}

/// Write `bytes` at an absolute file offset without changing the cursor.
#[cfg(not(any(unix, windows)))]
pub fn write_at(file: &File, offset: u64, bytes: &[u8]) -> Result<(), PlatformError> {
    use std::io::{Seek, SeekFrom, Write};

    let mut handle = file
        .try_clone()
        .map_err(|error| PlatformError::io_error("clone file handle", error))?;
    handle
        .seek(SeekFrom::Start(offset))
        .map_err(|error| PlatformError::io_error("seek file", error))?;
    handle
        .write_all(bytes)
        .map_err(|error| PlatformError::io_error("write at offset", error))
}

fn validate_relative(raw: &str) -> Result<PathBuf, PlatformError> {
    if raw.is_empty() {
        return Err(PlatformError::invalid_path("path must not be empty"));
    }
    if raw.contains('\0') {
        return Err(PlatformError::invalid_path("path must not contain NUL"));
    }
    if raw.contains('\\') {
        return Err(PlatformError::invalid_path(
            "path must not contain backslashes",
        ));
    }

    let path = Path::new(raw);
    if path.is_absolute() {
        return Err(PlatformError::invalid_path(format!(
            "path must be relative: {raw}"
        )));
    }

    let mut has_component = false;
    for component in path.components() {
        match component {
            Component::Normal(_) => has_component = true,
            _ => {
                return Err(PlatformError::invalid_path(format!(
                    "path must not contain root, prefix, parent, or current components: {raw}"
                )));
            }
        }
    }
    if !has_component {
        return Err(PlatformError::invalid_path(format!(
            "path has no usable component: {raw}"
        )));
    }

    Ok(path.to_path_buf())
}

fn final_is_symlink(path: &Path) -> bool {
    matches!(
        std::fs::symlink_metadata(path),
        Ok(metadata) if metadata.file_type().is_symlink()
    )
}

fn map_io(action: &str, raw: &str, error: std::io::Error) -> PlatformError {
    if error.kind() == std::io::ErrorKind::NotFound {
        PlatformError::not_found(format!("{action} {raw}: path does not exist"))
    } else {
        PlatformError::io_error(format!("{action} {raw}"), error)
    }
}
