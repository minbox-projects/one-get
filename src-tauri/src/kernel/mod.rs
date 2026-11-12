//! Runtime component kernel: manifest verification and installation primitives.

pub mod manifest;

pub use manifest::{canonicalize, verify_manifest, ManifestError, MAX_MANIFEST_BYTES};
