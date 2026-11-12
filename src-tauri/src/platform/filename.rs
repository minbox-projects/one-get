//! Deterministic filename sanitization for the supported desktop targets.

use unicode_normalization::UnicodeNormalization;

/// Filesystem family whose naming restrictions should be applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FilenameTarget {
    /// APFS/HFS+ rules: separators, colon, NUL, and control characters.
    MacOs,
    /// NTFS rules: reserved devices, illegal characters, trailing dots/spaces.
    Windows,
}

/// Maximum sanitized filename length in UTF-8 bytes.
pub const MAX_FILENAME_BYTES: usize = 200;

/// Normalize and sanitize a user-supplied filename for `target`.
///
/// The result is never empty, never a dot segment, never contains a path
/// separator or control character, and is at most [`MAX_FILENAME_BYTES`] bytes
/// on a UTF-8 character boundary. The transformation is deterministic.
pub fn sanitize_filename(input: &str, target: FilenameTarget) -> String {
    let normalized: String = input.nfc().collect();

    let mut output: String = normalized
        .chars()
        .map(|character| {
            if is_illegal(character, target) {
                '_'
            } else {
                character
            }
        })
        .collect();

    if matches!(target, FilenameTarget::Windows) {
        output = trim_windows_trailing(&output);
        output = prefix_reserved_device(&output);
    }

    output = truncate_utf8(&output, MAX_FILENAME_BYTES);

    if matches!(target, FilenameTarget::Windows) {
        output = trim_windows_trailing(&output);
        output = prefix_reserved_device(&output);
    }

    if output.is_empty() || output == "." || output == ".." {
        output = String::from("download");
    }

    output
}

fn is_illegal(character: char, target: FilenameTarget) -> bool {
    if matches!(character, '/' | '\\' | ':' | '\0') || character.is_control() {
        return true;
    }
    matches!(target, FilenameTarget::Windows)
        && matches!(character, '<' | '>' | '"' | '|' | '?' | '*')
}

fn trim_windows_trailing(value: &str) -> String {
    value.trim_end_matches(['.', ' ']).to_string()
}

fn prefix_reserved_device(value: &str) -> String {
    let stem = value.split('.').next().unwrap_or(value);
    if is_reserved_device(stem) {
        format!("_{value}")
    } else {
        value.to_string()
    }
}

fn is_reserved_device(stem: &str) -> bool {
    matches!(
        stem.to_ascii_uppercase().as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

fn truncate_utf8(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}
