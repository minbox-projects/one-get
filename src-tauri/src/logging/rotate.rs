//! Size-based JSON-line log rotation.
//!
//! A single current file is appended one complete line at a time. Before a
//! write would exceed the configured size, the current file is shifted to
//! `.1`, older files are shifted upward, and the oldest file beyond
//! `max_files - 1` rotations is removed. A line is never split across files, so
//! every file contains complete lines.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::AppError;

/// Default log file base name.
pub const LOG_FILE_NAME: &str = "oneget.log";

/// A rotating writer that keeps at most `max_files` files including the
/// current one.
pub struct RotatingWriter {
    directory: PathBuf,
    base_name: String,
    max_bytes: u64,
    max_files: usize,
    file: File,
    size: u64,
}

impl RotatingWriter {
    /// Open (creating if needed) the current log file.
    pub fn open(
        directory: PathBuf,
        base_name: impl Into<String>,
        max_bytes: u64,
        max_files: usize,
    ) -> Result<Self, AppError> {
        std::fs::create_dir_all(&directory).map_err(|error| {
            AppError::io_error(
                format!("create log directory {}", directory.display()),
                error,
            )
        })?;
        let base_name = base_name.into();
        let (file, size) = open_append(&directory.join(&base_name))?;
        Ok(Self {
            directory,
            base_name,
            max_bytes: max_bytes.max(1),
            max_files: max_files.max(1),
            file,
            size,
        })
    }

    /// Append one complete line, rotating first when the line would overflow.
    pub fn write_line(&mut self, line: &str) -> Result<(), AppError> {
        let bytes = line.as_bytes();
        let needed = bytes.len() as u64 + 1;
        if self.size > 0 && self.size + needed > self.max_bytes {
            self.rotate()?;
        }
        let path = self.current_path();
        self.file
            .write_all(bytes)
            .map_err(|error| AppError::io_error(format!("write log {}", path.display()), error))?;
        self.file
            .write_all(b"\n")
            .map_err(|error| AppError::io_error(format!("write log {}", path.display()), error))?;
        self.size += needed;
        Ok(())
    }

    /// Flush buffered writes to the operating system.
    pub fn flush(&mut self) -> Result<(), AppError> {
        let path = self.current_path();
        self.file
            .flush()
            .map_err(|error| AppError::io_error(format!("flush log {}", path.display()), error))
    }

    /// Path of the current log file.
    pub fn current_path(&self) -> PathBuf {
        self.directory.join(&self.base_name)
    }

    /// All managed log files, current file first.
    pub fn files(&self) -> Result<Vec<PathBuf>, AppError> {
        let mut paths = Vec::new();
        let current = self.current_path();
        if current.exists() {
            paths.push(current);
        }
        for index in 1..self.max_files {
            let path = self.rotated_path(index);
            if path.exists() {
                paths.push(path);
            }
        }
        Ok(paths)
    }

    fn rotate(&mut self) -> Result<(), AppError> {
        let current = self.current_path();
        self.flush()?;

        if self.max_files <= 1 {
            self.file = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&current)
                .map_err(|error| {
                    AppError::io_error(format!("truncate log {}", current.display()), error)
                })?;
            self.size = 0;
            return Ok(());
        }

        remove_if_exists(&self.rotated_path(self.max_files - 1))?;
        for index in (1..self.max_files - 1).rev() {
            rename_if_exists(&self.rotated_path(index), &self.rotated_path(index + 1))?;
        }
        rename_if_exists(&current, &self.rotated_path(1))?;

        let (file, size) = open_append(&current)?;
        self.file = file;
        self.size = size;
        Ok(())
    }

    fn rotated_path(&self, index: usize) -> PathBuf {
        self.directory.join(format!("{}.{index}", self.base_name))
    }
}

fn open_append(path: &Path) -> Result<(File, u64), AppError> {
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|error| AppError::io_error(format!("open log {}", path.display()), error))?;
    let size = file
        .metadata()
        .map_err(|error| AppError::io_error(format!("inspect log {}", path.display()), error))?
        .len();
    Ok((file, size))
}

fn remove_if_exists(path: &Path) -> Result<(), AppError> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::io_error(
            format!("remove rotated log {}", path.display()),
            error,
        )),
    }
}

fn rename_if_exists(source: &Path, destination: &Path) -> Result<(), AppError> {
    match std::fs::rename(source, destination) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(AppError::io_error(
            format!(
                "rotate log {} to {}",
                source.display(),
                destination.display()
            ),
            error,
        )),
    }
}
