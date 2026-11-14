//! Runtime component kernel: canonical JSON encoding, Ed25519 manifest
//! verification, typed catalog validation, and the verified catalog cache.

pub mod archive;
pub mod catalog;
pub mod download;
pub mod error;
pub mod install;
pub mod manifest;
pub mod source;
pub mod status;
pub mod store;
pub mod tools;

pub use catalog::{
    verify_catalog, ArchiveType, Artifact, ComponentCatalog, ComponentEntry, FileRole, Target,
    CATALOG_SCHEMA_VERSION,
};
pub use error::KernelError;
pub use install::{InstallOutcome, Installer};
pub use manifest::{canonicalize, verify_manifest, ManifestError, MAX_MANIFEST_BYTES};
pub use source::{configured_public_key, CatalogCache, CATALOG_FILE_NAME};
pub use status::{
    component_statuses, ComponentState, ComponentStatus, OperationRegistry, OperationState,
};
pub use store::{ComponentInstall, ComponentStore, InstalledFile, COMPONENTS_SCHEMA_VERSION};
pub use tools::{ResolvedTools, ToolRequirement, ToolResolver};
