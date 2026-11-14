//! Component operation state and the read-only status projection.
//!
//! The registry is shared with the interface through an `Arc`; each component
//! has at most one state, its percent is clamped to 100, and finishing an
//! operation removes the state so a later status read reports idle. The status
//! projection combines the active and installed records with an optional
//! verified catalog and never mutates an installation.

use std::collections::HashMap;
use std::sync::Mutex;

use super::catalog::{ComponentCatalog, Target};
use super::error::KernelError;
use super::store::ComponentStore;
use super::tools::verify_record;
use crate::platform::files::CapDir;

/// Phase and bounded progress of one component operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationState {
    /// Current phase label such as `downloading` or `activating`.
    pub phase: String,
    /// Progress from 0 through 100.
    pub percent: u8,
}

/// A thread-safe registry of in-flight component operations.
#[derive(Debug, Default)]
pub struct OperationRegistry {
    states: Mutex<HashMap<String, OperationState>>,
}

impl OperationRegistry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Start tracking `component` with an empty phase and zero percent.
    pub fn begin(&self, component: &str) {
        self.lock().insert(
            component.to_string(),
            OperationState {
                phase: String::new(),
                percent: 0,
            },
        );
    }

    /// Replace the phase and percent of an existing operation.
    ///
    /// `percent` is clamped to 100. An unknown component is ignored.
    pub fn update(&self, component: &str, phase: &str, percent: u8) {
        if let Some(state) = self.lock().get_mut(component) {
            state.phase = phase.to_string();
            state.percent = percent.min(100);
        }
    }

    /// Stop tracking `component`.
    pub fn finish(&self, component: &str) {
        self.lock().remove(component);
    }

    /// Return a snapshot of the current state for `component`.
    pub fn get(&self, component: &str) -> Option<OperationState> {
        self.lock().get(component).cloned()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, OperationState>> {
        self.states
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Stable state of one component.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComponentState {
    /// The active installation is verified and usable.
    Installed,
    /// No version is installed and a catalog is available.
    NotInstalled,
    /// A verified active installation has a newer catalog sequence.
    UpdateAvailable,
    /// A verified earlier installed version can be activated.
    RollbackAvailable,
    /// The active installation no longer matches its record.
    Tampered,
    /// The catalog has no artifact for the current target.
    Unsupported,
    /// No catalog is available and no active installation is verified.
    ManifestUnavailable,
}

/// Read-only projection of one component's installation state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComponentStatus {
    /// Component name.
    pub component: String,
    /// Current stable state.
    pub state: ComponentState,
    /// Active version label, when a record exists.
    pub active_version: Option<String>,
    /// Version label offered by the catalog, when available.
    pub available_version: Option<String>,
    /// Every installed version label, newest sequence first.
    pub installed_versions: Vec<String>,
    /// Earlier installed version that verifies for rollback, when present.
    pub rollback_version: Option<String>,
    /// Target key the projection was computed for.
    pub target: String,
    /// Typed verification or platform failure for a non-usable state.
    pub error: Option<KernelError>,
    /// In-flight operation state, when one is running.
    pub operation: Option<OperationState>,
}

/// Project the status of `yt-dlp`, `ffmpeg`, and `deno` in that stable order.
///
/// A verified active installation stays usable without a catalog. A catalog
/// that lacks the current target marks the component unsupported. Rollback is
/// offered only for an earlier recorded version whose target, version
/// directory, file set, size, and SHA-256 all verify.
pub fn component_statuses(
    store: &ComponentStore,
    kernel_root: &CapDir,
    catalog: Option<&ComponentCatalog>,
    target: Target,
    operations: &OperationRegistry,
) -> Result<Vec<ComponentStatus>, KernelError> {
    let mut statuses = Vec::with_capacity(3);
    for component in ["yt-dlp", "ffmpeg", "deno"] {
        statuses.push(component_status(
            store,
            kernel_root,
            catalog,
            target,
            operations,
            component,
        )?);
    }
    Ok(statuses)
}

fn component_status(
    store: &ComponentStore,
    kernel_root: &CapDir,
    catalog: Option<&ComponentCatalog>,
    target: Target,
    operations: &OperationRegistry,
    component: &str,
) -> Result<ComponentStatus, KernelError> {
    let active = store.active(component)?;
    let installed = store.installed(component)?;
    let entry = catalog.and_then(|catalog| catalog.components.get(component));

    let mut status = ComponentStatus {
        component: component.to_string(),
        state: ComponentState::ManifestUnavailable,
        active_version: active.as_ref().map(|record| record.version.clone()),
        available_version: entry.map(|entry| entry.display_version.clone()),
        installed_versions: installed
            .iter()
            .map(|record| record.version.clone())
            .collect(),
        rollback_version: None,
        target: target.as_str().to_string(),
        error: None,
        operation: operations.get(component),
    };

    let supported =
        catalog.is_some() && entry.is_some_and(|entry| entry.artifacts.contains_key(&target));
    if catalog.is_some() && !supported {
        status.state = ComponentState::Unsupported;
        status.error = Some(KernelError::new(
            "COMPONENT_UNSUPPORTED",
            format!(
                "component `{component}` has no artifact for target {}",
                target.as_str()
            ),
        ));
        return Ok(status);
    }

    let Some(active) = active else {
        status.state = if catalog.is_some() {
            ComponentState::NotInstalled
        } else {
            ComponentState::ManifestUnavailable
        };
        return Ok(status);
    };

    if let Err(error) = verify_record(kernel_root, target, &active) {
        status.state = if error.code() == "COMPONENT_UNSUPPORTED" {
            ComponentState::Unsupported
        } else {
            ComponentState::Tampered
        };
        status.error = Some(error);
        return Ok(status);
    }

    status.rollback_version = installed
        .iter()
        .filter(|record| record.sequence < active.sequence)
        .find(|record| verify_record(kernel_root, target, record).is_ok())
        .map(|record| record.version.clone());

    status.state = match entry {
        Some(entry) if entry.sequence > active.sequence => ComponentState::UpdateAvailable,
        _ if status.rollback_version.is_some() && catalog.is_some() => {
            ComponentState::RollbackAvailable
        }
        _ => ComponentState::Installed,
    };

    Ok(status)
}
