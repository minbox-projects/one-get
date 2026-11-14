//! Runtime component kernel: canonical JSON encoding, Ed25519 manifest
//! verification, typed catalog validation, and the verified catalog cache.

pub mod catalog;
pub mod error;
pub mod manifest;
pub mod source;

pub use catalog::{
    verify_catalog, ArchiveType, Artifact, ComponentCatalog, ComponentEntry, FileRole, Target,
    CATALOG_SCHEMA_VERSION,
};
pub use error::KernelError;
pub use manifest::{canonicalize, verify_manifest, ManifestError, MAX_MANIFEST_BYTES};
pub use source::{configured_public_key, CatalogCache, CATALOG_FILE_NAME};
