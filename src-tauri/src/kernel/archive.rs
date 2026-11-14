//! Declared-file-only extraction for component zip artifacts.
//!
//! The archive is read once into memory, every member is validated before any
//! write, and only declared regular files reach the capability directory.
//! Unsafe names, symbolic or special entries, duplicate members, undeclared
//! files, missing declared files, and entry, size, and compression-ratio limit
//! violations are rejected without touching the destination.

use std::collections::{HashMap, HashSet};
use std::io::{Cursor, Read, Write};
use std::path::{Component, Path};

use sha2::{Digest, Sha256};

use super::catalog::FileRole;
use super::error::KernelError;
use crate::platform::files::CapDir;

/// Bounds applied while inspecting an archive before extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveLimits {
    /// Maximum number of archive members, directories included.
    pub max_entries: usize,
    /// Maximum declared size of one member in bytes.
    pub max_file_bytes: u64,
    /// Maximum declared size of all members combined in bytes.
    pub max_total_bytes: u64,
    /// Maximum declared uncompressed-to-compressed ratio per member.
    pub max_compression_ratio: u64,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_entries: 32,
            max_file_bytes: 256 * 1024 * 1024,
            max_total_bytes: 512 * 1024 * 1024,
            max_compression_ratio: 200,
        }
    }
}

/// One declared file written by [`extract_declared_zip`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractedFile {
    /// Declared role the file fills.
    pub role: String,
    /// File name as written inside the destination.
    pub name: String,
    /// Lowercase hexadecimal SHA-256 of the written bytes.
    pub sha256: String,
    /// Written size in bytes.
    pub size: u64,
}

/// Extract the declared files of a stored or deflated zip archive.
///
/// Every rejection happens before the first write, so a refused archive leaves
/// no extracted file behind.
pub fn extract_declared_zip(
    archive_bytes: &[u8],
    declared: &[FileRole],
    destination: &CapDir,
    limits: &ArchiveLimits,
) -> Result<Vec<ExtractedFile>, KernelError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(archive_bytes))
        .map_err(|_| invalid("component archive is not a readable zip"))?;
    if archive.has_duplicate_names() {
        return Err(invalid("component archive contains a duplicate entry"));
    }
    let count = archive.len();
    if count > limits.max_entries {
        return Err(limit("component archive declares too many entries"));
    }

    let mut declared_by_name: HashMap<String, String> = HashMap::new();
    for file in declared {
        validate_name(&file.name)?;
        if declared_by_name
            .insert(file.name.clone(), file.role.clone())
            .is_some()
        {
            return Err(invalid("declared component file names must be unique"));
        }
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut total: u64 = 0;
    let mut payloads: Vec<(String, String, Vec<u8>)> = Vec::new();

    for index in 0..count {
        let mut entry = archive
            .by_index(index)
            .map_err(|_| invalid("component archive entry is unreadable"))?;
        let name = entry
            .name()
            .map_err(|_| invalid("component archive entry name is not valid UTF-8"))?
            .into_owned();
        validate_name(&name)?;
        if !seen.insert(name.clone()) {
            return Err(invalid("component archive contains a duplicate entry"));
        }
        if entry.is_symlink() {
            return Err(invalid("component archive contains a symbolic link"));
        }
        if let Some(mode) = entry.unix_mode() {
            let kind = mode & 0o170000;
            if kind != 0 && kind != 0o100_000 && kind != 0o040_000 {
                return Err(invalid(
                    "component archive contains a non-file, non-directory entry",
                ));
            }
        }

        let size = entry.size();
        let compressed = entry.compressed_size();
        if size > limits.max_file_bytes {
            return Err(limit("component archive member exceeds the per-file limit"));
        }
        total = total
            .checked_add(size)
            .ok_or_else(|| limit("component archive total size overflows"))?;
        if total > limits.max_total_bytes {
            return Err(limit("component archive exceeds the total size limit"));
        }
        if size > 0 {
            let expanded = compressed.saturating_mul(limits.max_compression_ratio);
            if compressed == 0 || size > expanded {
                return Err(limit(
                    "component archive member exceeds the compression ratio limit",
                ));
            }
        }

        if entry.is_dir() {
            continue;
        }
        let Some(role) = declared_by_name.get(&name) else {
            return Err(invalid("component archive contains an undeclared file"));
        };
        let role = role.clone();

        let mut data = Vec::new();
        let read_limit = size.saturating_add(1);
        entry
            .by_ref()
            .take(read_limit)
            .read_to_end(&mut data)
            .map_err(|_| invalid("component archive entry could not be read"))?;
        if data.len() as u64 != size {
            return Err(invalid(
                "component archive entry size does not match its declaration",
            ));
        }
        payloads.push((role, name, data));
    }

    if payloads.len() != declared_by_name.len()
        || declared_by_name
            .keys()
            .any(|name| !payloads.iter().any(|(_, found, _)| found == name))
    {
        return Err(invalid("component archive is missing a declared file"));
    }

    let mut extracted = Vec::with_capacity(payloads.len());
    for (role, name, data) in payloads {
        write_member(destination, &name, &data)?;
        extracted.push(ExtractedFile {
            role,
            name,
            sha256: hex::encode(Sha256::digest(&data)),
            size: data.len() as u64,
        });
    }
    Ok(extracted)
}

fn write_member(destination: &CapDir, name: &str, data: &[u8]) -> Result<(), KernelError> {
    let mut file = destination
        .open_rw_create(name)
        .map_err(|error| write_failed(error.to_string()))?;
    file.set_len(0)
        .map_err(|error| write_failed(format!("truncate {name}: {error}")))?;
    file.write_all(data)
        .map_err(|error| write_failed(format!("write {name}: {error}")))?;
    file.sync_all()
        .map_err(|error| write_failed(format!("sync {name}: {error}")))
}

fn validate_name(name: &str) -> Result<(), KernelError> {
    if name.is_empty() {
        return Err(invalid("component archive entry name is empty"));
    }
    if name.contains('\0') {
        return Err(invalid("component archive entry name contains NUL"));
    }
    if name.contains('\\') {
        return Err(invalid(
            "component archive entry name contains a backslash separator",
        ));
    }
    let path = Path::new(name);
    if path.is_absolute() {
        return Err(invalid("component archive entry name is absolute"));
    }
    let mut has_component = false;
    for component in path.components() {
        match component {
            Component::Normal(_) => has_component = true,
            _ => {
                return Err(invalid(
                    "component archive entry name contains a root, parent, or current component",
                ));
            }
        }
    }
    if !has_component {
        return Err(invalid(
            "component archive entry name has no usable component",
        ));
    }
    Ok(())
}

fn invalid(message: &str) -> KernelError {
    KernelError::new("COMPONENT_ARCHIVE_INVALID", message)
}

fn limit(message: &str) -> KernelError {
    KernelError::new("COMPONENT_ARCHIVE_LIMIT", message)
}

fn write_failed(message: String) -> KernelError {
    KernelError::new("COMPONENT_ARCHIVE_WRITE_FAILED", message)
}
