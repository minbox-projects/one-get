//! Preallocated part-file access on top of a capability directory.
//!
//! A part file lives beside its final destination and is named
//! `<filename>.oneget.part`. Workers write at absolute byte offsets so a
//! segment can resume where it stopped, and finalization hashes the finished
//! bytes before the part is renamed.

#[cfg(debug_assertions)]
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::ops::Deref;
#[cfg(debug_assertions)]
use std::sync::Mutex;

use sha2::{Digest, Sha256};

use crate::platform::files::{write_at, CapDir, SparsePreallocation};
use crate::platform::PlatformError;

use super::error::EngineError;

/// Suffix appended to a destination filename to form its part file.
const PART_SUFFIX: &str = ".oneget.part";

/// Cumulative write budget installed by a test to simulate a full disk.
#[cfg(debug_assertions)]
#[derive(Debug, Default)]
struct WriteFault {
    fail_after: u64,
    written: u64,
}

/// Per-part write counters used only by debug-build fault injection.
#[cfg(debug_assertions)]
static TEST_WRITE_FAULTS: Mutex<Option<HashMap<String, WriteFault>>> = Mutex::new(None);

/// Install a debug-only write fault for one part file.
///
/// Writes fail with `DISK_FULL` once the cumulative written bytes would exceed
/// `fail_after_bytes`. Release builds ignore the request.
#[cfg(debug_assertions)]
pub fn set_test_write_fault(part_name: &str, fail_after_bytes: u64) {
    let mut guard = TEST_WRITE_FAULTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.get_or_insert_with(HashMap::new).insert(
        part_name.to_string(),
        WriteFault {
            fail_after: fail_after_bytes,
            written: 0,
        },
    );
}

/// Install a debug-only write fault for one part file.
#[cfg(not(debug_assertions))]
pub fn set_test_write_fault(_part_name: &str, _fail_after_bytes: u64) {}

/// Clear a previously installed debug-only write fault.
#[cfg(debug_assertions)]
pub fn clear_test_write_fault(part_name: &str) {
    let mut guard = TEST_WRITE_FAULTS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(faults) = guard.as_mut() {
        faults.remove(part_name);
    }
}

/// Clear a previously installed debug-only write fault.
#[cfg(not(debug_assertions))]
pub fn clear_test_write_fault(_part_name: &str) {}

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
    preallocation: SparsePreallocation,
    cap: CapDir,
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
    /// A known total is preallocated through the platform layer, which attempts
    /// a sparse allocation and reports whether it succeeded. An unknown total
    /// leaves the file empty until bytes arrive.
    pub fn create(cap: &CapDir, filename: &str, total: Option<u64>) -> Result<Self, EngineError> {
        Self::create_named(cap, &Self::part_name(filename), total)
    }

    /// Create or reopen a part file under an explicit persisted part name.
    pub fn create_named(cap: &CapDir, name: &str, total: Option<u64>) -> Result<Self, EngineError> {
        let name = name.to_string();
        let file = cap.open_rw_create(&name).map_err(platform_error)?;
        let preallocation = match total {
            Some(len) => cap
                .preallocate_sparse(&name, len)
                .map_err(map_preallocate_error)?,
            None => SparsePreallocation::Fallback,
        };
        Ok(Self {
            file,
            name: PartFileName(name),
            preallocation,
            cap: cap.clone(),
        })
    }

    /// Open an existing part file without creating it.
    pub fn open(cap: &CapDir, filename: &str) -> Result<Self, EngineError> {
        Self::open_named(cap, &Self::part_name(filename))
    }

    /// Open an existing part file under an explicit persisted part name.
    pub fn open_named(cap: &CapDir, name: &str) -> Result<Self, EngineError> {
        let name = name.to_string();
        let file = cap.open_rw(&name).map_err(platform_error)?;
        Ok(Self {
            file,
            name: PartFileName(name),
            preallocation: SparsePreallocation::Fallback,
            cap: cap.clone(),
        })
    }

    /// How the part file was preallocated when it was created.
    pub fn preallocation_outcome(&self) -> SparsePreallocation {
        self.preallocation
    }

    /// Extend the part file to `total` bytes through the capability.
    ///
    /// The platform attempts a sparse allocation first. A full destination
    /// device maps to `DISK_FULL`; other failures map to `IO_ERROR`.
    pub fn preallocate(&self, total: u64) -> Result<SparsePreallocation, EngineError> {
        self.cap
            .preallocate_sparse(self.name.part_name(), total)
            .map_err(map_preallocate_error)
    }

    /// Write `bytes` at an absolute offset in the part file.
    ///
    /// A full destination device maps to `DISK_FULL`; other write failures map
    /// to `IO_ERROR`. Debug builds also honor an installed cumulative fault so
    /// tests can exercise the disk-full path without filling a real volume.
    pub fn write_at(&self, offset: u64, bytes: &[u8]) -> Result<(), EngineError> {
        #[cfg(debug_assertions)]
        if self.test_write_limit_reached(bytes.len() as u64) {
            return Err(EngineError::disk_full());
        }
        write_at(&self.file, offset, bytes).map_err(map_write_error)
    }

    #[cfg(debug_assertions)]
    fn test_write_limit_reached(&self, length: u64) -> bool {
        let mut guard = TEST_WRITE_FAULTS
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(faults) = guard.as_mut() else {
            return false;
        };
        let Some(fault) = faults.get_mut(self.name.0.as_str()) else {
            return false;
        };
        if fault.written.saturating_add(length) > fault.fail_after {
            return true;
        }
        fault.written = fault.written.saturating_add(length);
        false
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

fn map_preallocate_error(error: PlatformError) -> EngineError {
    if is_storage_error(&error.message) {
        EngineError::disk_full()
    } else {
        EngineError::new(
            "IO_ERROR",
            format!("the part file could not be sized: {}", error.message),
        )
    }
}

fn map_write_error(error: PlatformError) -> EngineError {
    if is_storage_error(&error.message) {
        EngineError::disk_full()
    } else {
        EngineError::new("IO_ERROR", "the part file could not be written")
    }
}

/// The platform layer reports the operating-system text rather than the raw
/// error kind, so recognize the common out-of-space spellings explicitly.
fn is_storage_error(message: &str) -> bool {
    let message = message.to_ascii_lowercase();
    [
        "no space left",
        "not enough space",
        "disk full",
        "os error 28",
        "os error 112",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}
