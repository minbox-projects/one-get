//! Component installation, update, rollback, and verified resolution support.
//!
//! An install refuses to run without a configured trust key, selects only the
//! current target's artifact, downloads it through the bounded artifact client,
//! stages the declared files inside a hidden directory under the kernel root,
//! prepares them for execution, and activates the staged directory with one
//! directory rename. The active record is written only after activation, and any
//! failure removes the staged or newly activated directory while leaving the
//! previous active version untouched.
//!
//! Rollback never touches the network: it selects the highest installed
//! sequence below the active one, verifies the recorded target, file set, size,
//! and SHA-256 through the capability root, and only then switches the active
//! pointer.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::archive::{extract_declared_zip, ArchiveLimits, ExtractedFile};
use super::catalog::{ArchiveType, Artifact, ComponentCatalog, ComponentEntry, Target};
use super::download::download_artifact_with_proxy;
use super::error::KernelError;
use super::status::OperationRegistry;
use super::store::{ComponentInstall, ComponentStore, InstalledFile};
use crate::engine::http::ProxyConfig;
use crate::platform::component::prepare_executable;
use crate::platform::files::CapDir;
use crate::platform::PlatformError;

/// Upper bound for one artifact download.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(60);

/// Result of an update request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    /// A strictly newer sequence was installed and activated.
    Installed(ComponentInstall),
    /// The catalog sequence already matches the active version.
    UpToDate(ComponentInstall),
}

/// Installs, updates, and rolls back component versions.
pub struct Installer {
    store: ComponentStore,
    kernel_root: CapDir,
    target: Target,
    public_key: Option<String>,
    allow_loopback_http: bool,
    proxy: Option<ProxyConfig>,
    operations: Option<Arc<OperationRegistry>>,
}

impl Installer {
    /// Create an installer over a component store and kernel root.
    pub fn new(
        store: ComponentStore,
        kernel_root: CapDir,
        target: Target,
        public_key: Option<String>,
        allow_loopback_http: bool,
    ) -> Self {
        Self {
            store,
            kernel_root,
            target,
            public_key,
            allow_loopback_http,
            proxy: None,
            operations: None,
        }
    }

    /// Set the effective proxy used by later artifact downloads.
    pub fn set_proxy(&mut self, proxy: Option<ProxyConfig>) {
        self.proxy = proxy;
    }

    /// Attach the shared operation registry used by the interface.
    pub fn set_operations(&mut self, operations: Arc<OperationRegistry>) {
        self.operations = Some(operations);
    }

    /// Install `component` from a verified catalog.
    pub async fn install(
        &self,
        catalog: &ComponentCatalog,
        component: &str,
    ) -> Result<ComponentInstall, KernelError> {
        self.begin_operation(component);
        let result = self.install_inner(catalog, component).await;
        self.finish_operation(component);
        result
    }

    /// Install a strictly newer sequence, or report the active version as
    /// up to date when the sequence is equal.
    pub async fn update(
        &self,
        catalog: &ComponentCatalog,
        component: &str,
    ) -> Result<InstallOutcome, KernelError> {
        self.begin_operation(component);
        let result = self.update_inner(catalog, component).await;
        self.finish_operation(component);
        result
    }

    /// Switch to the highest installed sequence below the active sequence.
    ///
    /// The recorded target, file set, size, and SHA-256 are verified before the
    /// active pointer moves, and no network request is made.
    pub async fn rollback(&self, component: &str) -> Result<ComponentInstall, KernelError> {
        self.begin_operation(component);
        let result = self.rollback_inner(component).await;
        self.finish_operation(component);
        result
    }

    async fn install_inner(
        &self,
        catalog: &ComponentCatalog,
        component: &str,
    ) -> Result<ComponentInstall, KernelError> {
        self.require_trust()?;
        let (entry, artifact) = self.select_artifact(catalog, component)?;
        let sequence = entry.sequence;
        let display_version = entry.display_version.clone();

        self.update_operation(component, "downloading", 10);
        let bytes = download_artifact_with_proxy(
            &artifact.url,
            &artifact.sha256,
            artifact.size,
            self.allow_loopback_http,
            DOWNLOAD_TIMEOUT,
            self.proxy.as_ref(),
        )
        .await?;
        self.update_operation(component, "verifying", 50);

        let work_rel = format!(".work-{}", Uuid::new_v4());
        let work = TempWork::new(self.kernel_root.root(), &work_rel);
        let staging_rel = format!("{work_rel}/version");

        let extracted = {
            let staging = CapDir::open(self.kernel_root.root().join(&staging_rel))
                .map_err(platform_failed)?;
            let files = stage_artifact(artifact, &bytes, &staging).await?;
            self.update_operation(component, "preparing", 70);
            for file in &files {
                prepare_executable(&staging, &file.name).map_err(platform_failed)?;
            }
            files
        };

        let version_dir = version_directory(component, sequence, &display_version);
        self.update_operation(component, "activating", 85);
        self.kernel_root
            .create_dir_all(component)
            .map_err(platform_failed)?;
        self.kernel_root
            .rename(&staging_rel, &version_dir)
            .map_err(platform_failed)?;
        let _ = self.kernel_root.remove_dir(&work_rel);

        let record = ComponentInstall {
            component: component.to_string(),
            version: display_version,
            sequence,
            target: self.target.as_str().to_string(),
            version_dir,
            files: extracted
                .into_iter()
                .map(|file| InstalledFile {
                    role: file.role,
                    name: file.name,
                    sha256: file.sha256,
                    size: file.size,
                })
                .collect(),
            installed_at_ms: now_ms(),
        };

        if let Err(error) = self.store.record_active(&record) {
            let _ = std::fs::remove_dir_all(self.kernel_root.root().join(&record.version_dir));
            return Err(error);
        }

        self.update_operation(component, "installed", 100);
        drop(work);
        Ok(record)
    }

    async fn update_inner(
        &self,
        catalog: &ComponentCatalog,
        component: &str,
    ) -> Result<InstallOutcome, KernelError> {
        let entry = catalog
            .components
            .get(component)
            .ok_or_else(|| invalid(&format!("catalog does not declare component `{component}`")))?;

        let Some(active) = self.store.active(component)? else {
            let installed = self.install_inner(catalog, component).await?;
            return Ok(InstallOutcome::Installed(installed));
        };

        if entry.sequence == active.sequence {
            return Ok(InstallOutcome::UpToDate(active));
        }
        if entry.sequence < active.sequence {
            return Err(KernelError::new(
                "COMPONENT_VERSION_MISMATCH",
                "the catalog sequence is lower than the active sequence",
            ));
        }

        let installed = self.install_inner(catalog, component).await?;
        Ok(InstallOutcome::Installed(installed))
    }

    async fn rollback_inner(&self, component: &str) -> Result<ComponentInstall, KernelError> {
        let active = self.store.active(component)?.ok_or_else(|| {
            KernelError::new(
                "COMPONENT_MISSING",
                format!("component `{component}` has no active version"),
            )
        })?;

        let candidate = self
            .store
            .installed(component)?
            .into_iter()
            .filter(|record| record.sequence < active.sequence)
            .max_by_key(|record| record.sequence)
            .ok_or_else(|| {
                KernelError::new(
                    "COMPONENT_ROLLBACK_UNAVAILABLE",
                    format!("component `{component}` has no earlier installed version"),
                )
            })?;

        self.verify_installed(&candidate).await?;
        self.store.record_active(&candidate)?;
        Ok(candidate)
    }

    async fn verify_installed(&self, record: &ComponentInstall) -> Result<(), KernelError> {
        if record.target != self.target.as_str() {
            return Err(KernelError::new(
                "COMPONENT_UNSUPPORTED",
                "the installed version was recorded for another target",
            ));
        }

        let directory = self.kernel_root.root().join(&record.version_dir);
        let metadata = std::fs::symlink_metadata(&directory).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                missing_file()
            } else {
                install_failed(error.to_string())
            }
        })?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(missing_file());
        }

        let mut found: Vec<String> = std::fs::read_dir(&directory)
            .map_err(|error| install_failed(error.to_string()))?
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                entry
                    .file_type()
                    .ok()
                    .filter(|file_type| file_type.is_file())
                    .map(|_| entry.file_name().to_string_lossy().into_owned())
            })
            .collect();
        found.sort_unstable();
        let mut expected: Vec<String> = record.files.iter().map(|file| file.name.clone()).collect();
        expected.sort_unstable();
        if found != expected {
            return Err(KernelError::new(
                "COMPONENT_HASH",
                "the installed version file set does not match its record",
            ));
        }

        for file in &record.files {
            let path = format!("{}/{}", record.version_dir, file.name);
            let data = self.kernel_root.read(&path).await.map_err(|error| {
                if error.code == "not_found" {
                    missing_file()
                } else {
                    install_failed(error.to_string())
                }
            })?;
            let digest = hex::encode(Sha256::digest(&data));
            if data.len() as u64 != file.size || digest != file.sha256 {
                return Err(KernelError::new(
                    "COMPONENT_HASH",
                    "the installed version does not match its recorded hash or size",
                ));
            }
        }

        Ok(())
    }

    fn require_trust(&self) -> Result<(), KernelError> {
        match self.public_key.as_deref() {
            Some(key) if !key.is_empty() => Ok(()),
            _ => Err(KernelError::new(
                "COMPONENT_TRUST_UNCONFIGURED",
                "no component trust public key is configured",
            )),
        }
    }

    fn select_artifact<'a>(
        &self,
        catalog: &'a ComponentCatalog,
        component: &str,
    ) -> Result<(&'a ComponentEntry, &'a Artifact), KernelError> {
        let entry = catalog
            .components
            .get(component)
            .ok_or_else(|| invalid(&format!("catalog does not declare component `{component}`")))?;
        let artifact = entry.artifacts.get(&self.target).ok_or_else(|| {
            KernelError::new(
                "COMPONENT_UNSUPPORTED",
                format!(
                    "component `{component}` has no artifact for target {}",
                    self.target.as_str()
                ),
            )
        })?;
        Ok((entry, artifact))
    }

    fn begin_operation(&self, component: &str) {
        if let Some(operations) = &self.operations {
            operations.begin(component);
        }
    }

    fn update_operation(&self, component: &str, phase: &str, percent: u8) {
        if let Some(operations) = &self.operations {
            operations.update(component, phase, percent);
        }
    }

    fn finish_operation(&self, component: &str) {
        if let Some(operations) = &self.operations {
            operations.finish(component);
        }
    }
}

async fn stage_artifact(
    artifact: &Artifact,
    bytes: &[u8],
    staging: &CapDir,
) -> Result<Vec<ExtractedFile>, KernelError> {
    match artifact.archive {
        ArchiveType::Zip => {
            extract_declared_zip(bytes, &artifact.files, staging, &ArchiveLimits::default())
        }
        ArchiveType::Binary => {
            if artifact.files.len() != 1 {
                return Err(invalid(
                    "a binary artifact must declare exactly one output file",
                ));
            }
            let file = &artifact.files[0];
            staging
                .write(&file.name, bytes)
                .await
                .map_err(platform_failed)?;
            Ok(vec![ExtractedFile {
                role: file.role.clone(),
                name: file.name.clone(),
                sha256: hex::encode(Sha256::digest(bytes)),
                size: bytes.len() as u64,
            }])
        }
    }
}

/// Remove a hidden staging directory when an install does not complete.
struct TempWork {
    path: PathBuf,
}

impl TempWork {
    fn new(root: &Path, relative: &str) -> Self {
        Self {
            path: root.join(relative),
        }
    }
}

impl Drop for TempWork {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn version_directory(component: &str, sequence: u64, display_version: &str) -> String {
    format!(
        "{component}/{sequence}-{}",
        sanitize_display_version(display_version)
    )
}

fn sanitize_display_version(version: &str) -> String {
    let mut sanitized = String::with_capacity(version.len());
    for character in version.chars() {
        if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
            sanitized.push(character);
        } else {
            sanitized.push('-');
        }
    }
    let trimmed = sanitized.trim_matches(|character| character == '-' || character == '.');
    if trimmed.is_empty() {
        "version".to_string()
    } else {
        trimmed.to_string()
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

fn platform_failed(error: PlatformError) -> KernelError {
    install_failed(error.to_string())
}

fn install_failed(message: impl Into<String>) -> KernelError {
    KernelError::new("COMPONENT_INSTALL_FAILED", message)
}

fn missing_file() -> KernelError {
    KernelError::new(
        "COMPONENT_MISSING",
        "an installed component file is missing",
    )
}

fn invalid(message: &str) -> KernelError {
    KernelError::new("COMPONENT_CATALOG_INVALID", message)
}
