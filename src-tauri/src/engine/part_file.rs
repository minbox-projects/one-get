//! Preallocated part-file access on top of a capability directory.
//!
//! A part file lives beside its final destination and is named
//! `<filename>.oneget.part`. Workers write at absolute byte offsets so a
//! segment can resume where it stopped, and finalization hashes the finished
//! bytes before the part is renamed.

use std::io::{Read, Seek, SeekFrom};
use std::ops::Deref;

use sha2::{Digest, Sha256};

use crate::platform::files::{write_at, CapDir};
use crate::platform::PlatformError;

use super::error::EngineError;

/// Suffix appended to a destination filename to form its part file.
const PART_SUFFIX: &str = ".oneget.part";

/// Name carried by a [`PartFile`], also the method lookup target for the
/// instance-level `part_name` accessor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PartFileName(String);

impl PartFileName {
    /// The part file name owned by this part.
    pub fn part_name(&self) -> &str {
        &self.0
    }
}

/// A preallocated part file that accepts absolute-offset writes.
#[derive(Debug)]
pub struct PartFile {
    file: std::fs::File,
    name: PartFileName,
}

impl Deref for PartFile {
    type Target = PartFileName;

    fn deref(&self) -> &Self::Target {
        &self.name
    }
}

impl PartFile {
    /// Part file name for `filename`, for example `payload.bin.oneget.part`.
    pub fn part_name(filename: &str) -> String {
        format!("{filename}{PART_SUFFIX}")
    }

    /// Create or reopen the part file for `filename`.
    ///
    /// A known total is preallocated with `set_len`, which produces a sparse
    /// file on macOS. An unknown total is left empty until bytes arrive.
    pub fn create(cap: &CapDir, filename: &str, total: Option<u64>) -> Result<Self, EngineError> {
        let name = Self::part_name(filename);
        let file = cap.open_rw_create(&name).map_err(platform_error)?;
        if let Some(len) = total {
            file.set_len(len).map_err(|error| {
                EngineError::new(
                    "IO_ERROR",
                    format!("the part file could not be sized: {error}"),
                )
            })?;
        }
        Ok(Self {
            file,
            name: PartFileName(name),
        })
    }

    /// Open an existing part file without creating it.
    pub fn open(cap: &CapDir, filename: &str) -> Result<Self, EngineError> {
        let name = Self::part_name(filename);
        let file = cap.open_rw(&name).map_err(platform_error)?;
        Ok(Self {
            file,
            name: PartFileName(name),
        })
    }

    /// Write `bytes` at an absolute offset in the part file.
    pub fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<(), EngineError> {
        write_at(&self.file, offset, bytes).map_err(platform_error)
    }

    /// Flush the part file contents to durable storage.
    pub fn sync_all(&self) -> Result<(), EngineError> {
        self.file.sync_all().map_err(|error| {
            EngineError::new(
                "IO_ERROR",
                format!("the part file could not be synced: {error}"),
            )
        })
    }

    /// Current length of the part file in bytes.
    pub fn len(&self) -> u64 {
        self.file
            .metadata()
            .map(|metadata| metadata.len())
            .unwrap_or(0)
    }

    /// Whether the part file has no bytes.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Streaming SHA-256 of the part file as lowercase hexadecimal.
    ///
    /// Read errors yield an empty digest; callers that must distinguish a real
    /// hash from an I/O failure use [`PartFile::try_sha256`].
    pub fn sha256(&self) -> String {
        self.try_sha256().unwrap_or_default()
    }

    /// Streaming SHA-256 of the part file, reporting read failures.
    pub fn try_sha256(&self) -> Result<String, EngineError> {
        let mut reader = self.file.try_clone().map_err(|error| {
            EngineError::new(
                "IO_ERROR",
                format!("the part file could not be cloned: {error}"),
            )
        })?;
        reader.seek(SeekFrom::Start(0)).map_err(|error| {
            EngineError::new(
                "IO_ERROR",
                format!("the part file could not be read: {error}"),
            )
        })?;

        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = reader.read(&mut buffer).map_err(|error| {
                EngineError::new(
                    "IO_ERROR",
                    format!("the part file could not be read: {error}"),
                )
            })?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(hex::encode(hasher.finalize()))
    }

    /// Underlying handle for platform-level operations.
    pub fn file(&self) -> &std::fs::File {
        &self.file
    }
}

fn platform_error(error: PlatformError) -> EngineError {
    EngineError::new(error.code, error.message)
}
