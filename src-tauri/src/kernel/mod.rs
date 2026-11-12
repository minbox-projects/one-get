//! Runtime component kernel: canonical JSON encoding and Ed25519 manifest verification.

pub mod manifest;

pub use manifest::{canonicalize, verify_manifest, ManifestError, MAX_MANIFEST_BYTES};
