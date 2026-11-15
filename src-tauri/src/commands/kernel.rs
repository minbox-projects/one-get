//! Runtime component kernel behind the Tauri command surface.
//!
//! [`KernelRuntime`] validates trust, the cached catalog, and the per-component
//! busy flag synchronously, then runs install, update, and rollback on a worker
//! thread with a current-thread Tokio runtime. The last failed operation is
//! retained per component and overlaid onto the status projection; the active
//! pointer only moves after a successful operation.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::app_state::AppState;
use crate::engine::http::ProxyConfig;
use crate::error::AppError;
use crate::kernel::catalog::{ComponentCatalog, Target};
use crate::kernel::install::Installer;
use crate::kernel::source::{fetch_catalog_with_proxy, manifest_url_override, CatalogCache};
use crate::kernel::status::{
    component_statuses, ComponentState, ComponentStatus, OperationRegistry, OperationState,
};
use crate::kernel::store::ComponentStore;
use crate::kernel::KernelError;
use crate::platform::files::CapDir;

/// Upper bound for one manifest refresh.
const CATALOG_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Copy)]
enum OperationKind {
    Install,
    Update,
    Rollback,
}

/// Runtime component kernel shared by the command surface.
pub struct KernelRuntime {
    components_db: PathBuf,
    kernel_root: PathBuf,
    cache_dir: PathBuf,
    trust: Option<String>,
    manifest_url: Option<String>,
    target: Target,
    allow_loopback_http: bool,
    proxy: Mutex<Option<ProxyConfig>>,
    operations: Arc<OperationRegistry>,
    busy: Arc<Mutex<HashSet<String>>>,
    last_errors: Arc<Mutex<HashMap<String, KernelError>>>,
    catalog: Arc<Mutex<Option<ComponentCatalog>>>,
}

impl KernelRuntime {
    /// Create a runtime and load any verified cached catalog when trust allows.
    pub fn new(
        components_db: PathBuf,
        kernel_root: PathBuf,
        cache_dir: PathBuf,
        trust: Option<String>,
        manifest_url: Option<String>,
        target: Target,
        allow_loopback_http: bool,
    ) -> Result<Arc<Self>, KernelError> {
        let runtime = Arc::new(Self {
            components_db,
            kernel_root,
            cache_dir,
            trust: trust.filter(|key| !key.is_empty()),
            manifest_url,
            target,
            allow_loopback_http,
            proxy: Mutex::new(None),
            operations: Arc::new(OperationRegistry::new()),
            busy: Arc::new(Mutex::new(HashSet::new())),
            last_errors: Arc::new(Mutex::new(HashMap::new())),
            catalog: Arc::new(Mutex::new(None)),
        });
        if let Some(key) = runtime.trust.as_deref() {
            if let Ok(catalog) = CatalogCache::new(&runtime.cache_dir).load(
                Some(key),
                target,
                runtime.allow_loopback_http,
            ) {
                *runtime.lock_catalog() = Some(catalog);
            }
        }
        Ok(runtime)
    }

    /// Replace the proxy used by later catalog refreshes and installs.
    ///
    /// An operation that already started keeps the proxy it captured.
    pub fn set_proxy(&self, proxy: Option<ProxyConfig>) {
        *self
            .proxy
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = proxy;
    }

    fn proxy(&self) -> Option<ProxyConfig> {
        self.proxy
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Fetch, verify, and atomically cache the signed catalog.
    pub fn refresh(&self) -> Result<(), KernelError> {
        let trust = self.trust.as_deref().ok_or_else(trust_unconfigured)?;
        let url = self
            .manifest_url
            .clone()
            .or_else(manifest_url_override)
            .ok_or_else(manifest_unavailable)?;
        let proxy = self.proxy();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| manifest_unavailable())?;
        let bytes = runtime.block_on(fetch_catalog_with_proxy(
            &url,
            self.allow_loopback_http,
            CATALOG_TIMEOUT,
            proxy.as_ref(),
        ))?;
        let catalog = CatalogCache::new(&self.cache_dir).store_verified(
            &bytes,
            Some(trust),
            self.target,
            self.allow_loopback_http,
        )?;
        *self.lock_catalog() = Some(catalog);
        Ok(())
    }

    /// Project the stable status of every component with operation state.
    pub fn statuses(&self) -> Result<Vec<ComponentStatus>, KernelError> {
        let store = ComponentStore::open(&self.components_db)?;
        let root = CapDir::open(&self.kernel_root).map_err(platform_error)?;
        let catalog = self.lock_catalog().clone();
        let mut statuses = component_statuses(
            &store,
            &root,
            catalog.as_ref(),
            self.target,
            &self.operations,
        )?;
        let errors = self.lock_errors();
        for status in &mut statuses {
            if let Some(error) = errors.get(&status.component) {
                status.error = Some(error.clone());
            }
        }
        Ok(statuses)
    }

    /// Start a background install of `component`.
    pub fn install(&self, component: &str) -> Result<(), KernelError> {
        self.require_trust()?;
        let catalog = self
            .lock_catalog()
            .clone()
            .ok_or_else(manifest_unavailable)?;
        self.start(component, OperationKind::Install, Some(catalog))
    }

    /// Start a background update of `component`.
    pub fn update(&self, component: &str) -> Result<(), KernelError> {
        self.require_trust()?;
        let catalog = self
            .lock_catalog()
            .clone()
            .ok_or_else(manifest_unavailable)?;
        self.start(component, OperationKind::Update, Some(catalog))
    }

    /// Start a background rollback that never touches the network.
    pub fn rollback(&self, component: &str) -> Result<(), KernelError> {
        self.start(component, OperationKind::Rollback, None)
    }

    /// Current in-flight operation for `component`.
    pub fn operation(&self, component: &str) -> Option<OperationState> {
        self.operations.get(component)
    }

    /// Most recent failed operation for `component`.
    pub fn last_error(&self, component: &str) -> Option<KernelError> {
        self.lock_errors().get(component).cloned()
    }

    /// Wait until the background operation stops, returning its failure.
    pub fn wait_until_idle(&self, component: &str, timeout: Duration) -> Result<(), KernelError> {
        let deadline = Instant::now() + timeout;
        loop {
            if !self.lock_busy().contains(component) {
                return match self.last_error(component) {
                    Some(error) => Err(error),
                    None => Ok(()),
                };
            }
            if Instant::now() >= deadline {
                return Err(KernelError::new(
                    "COMPONENT_TIMEOUT",
                    "the component operation did not finish in time",
                ));
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn require_trust(&self) -> Result<(), KernelError> {
        match self.trust.as_deref() {
            Some(key) if !key.is_empty() => Ok(()),
            _ => Err(trust_unconfigured()),
        }
    }

    fn start(
        &self,
        component: &str,
        kind: OperationKind,
        catalog: Option<ComponentCatalog>,
    ) -> Result<(), KernelError> {
        {
            let mut busy = self.lock_busy();
            if busy.contains(component) {
                return Err(KernelError::new(
                    "COMPONENT_BUSY",
                    format!("component `{component}` already has a running operation"),
                ));
            }
            busy.insert(component.to_string());
        }
        self.lock_errors().remove(component);

        let components_db = self.components_db.clone();
        let kernel_root = self.kernel_root.clone();
        let trust = self.trust.clone();
        let target = self.target;
        let allow_loopback_http = self.allow_loopback_http;
        let proxy = self.proxy();
        let operations = Arc::clone(&self.operations);
        let busy = Arc::clone(&self.busy);
        let last_errors = Arc::clone(&self.last_errors);
        let name = component.to_string();

        let spawned = thread::Builder::new()
            .name(format!("oneget-kernel-{component}"))
            .spawn(move || {
                let result = run_operation(OperationJob {
                    components_db,
                    kernel_root,
                    trust,
                    target,
                    allow_loopback_http,
                    proxy,
                    operations,
                    catalog,
                    kind,
                    component: name.clone(),
                });
                if let Err(error) = result {
                    last_errors
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .insert(name.clone(), error);
                }
                busy.lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .remove(&name);
            });

        if spawned.is_err() {
            self.lock_busy().remove(component);
            return Err(KernelError::new(
                "COMPONENT_INSTALL_FAILED",
                "the component worker thread could not start",
            ));
        }
        Ok(())
    }

    fn lock_busy(&self) -> std::sync::MutexGuard<'_, HashSet<String>> {
        self.busy
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_errors(&self) -> std::sync::MutexGuard<'_, HashMap<String, KernelError>> {
        self.last_errors
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn lock_catalog(&self) -> std::sync::MutexGuard<'_, Option<ComponentCatalog>> {
        self.catalog
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

struct OperationJob {
    components_db: PathBuf,
    kernel_root: PathBuf,
    trust: Option<String>,
    target: Target,
    allow_loopback_http: bool,
    proxy: Option<ProxyConfig>,
    operations: Arc<OperationRegistry>,
    catalog: Option<ComponentCatalog>,
    kind: OperationKind,
    component: String,
}

fn run_operation(job: OperationJob) -> Result<(), KernelError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| {
            KernelError::new(
                "COMPONENT_INSTALL_FAILED",
                "the component runtime could not start",
            )
        })?;
    let OperationJob {
        components_db,
        kernel_root,
        trust,
        target,
        allow_loopback_http,
        proxy,
        operations,
        catalog,
        kind,
        component,
    } = job;
    runtime.block_on(async move {
        let store = ComponentStore::open(&components_db)?;
        let root = CapDir::open(&kernel_root).map_err(platform_error)?;
        let mut installer = Installer::new(store, root, target, trust, allow_loopback_http);
        installer.set_proxy(proxy);
        installer.set_operations(operations);
        match kind {
            OperationKind::Install => {
                let catalog = catalog.ok_or_else(manifest_unavailable)?;
                installer.install(&catalog, &component).await.map(|_| ())
            }
            OperationKind::Update => {
                let catalog = catalog.ok_or_else(manifest_unavailable)?;
                installer.update(&catalog, &component).await.map(|_| ())
            }
            OperationKind::Rollback => installer.rollback(&component).await.map(|_| ()),
        }
    })
}

fn trust_unconfigured() -> KernelError {
    KernelError::new(
        "COMPONENT_TRUST_UNCONFIGURED",
        "no component trust public key is configured",
    )
}

fn manifest_unavailable() -> KernelError {
    KernelError::new(
        "COMPONENT_MANIFEST_UNAVAILABLE",
        "no verified component catalog is available",
    )
}

fn platform_error(error: crate::platform::PlatformError) -> KernelError {
    KernelError::new(error.code, error.message)
}

fn app_error(error: KernelError) -> AppError {
    AppError::new(error.code(), error.message())
}

fn runtime(state: &AppState) -> Result<&KernelRuntime, AppError> {
    state.kernel().ok_or_else(|| {
        AppError::new(
            "KERNEL_UNAVAILABLE",
            "the component kernel is not available",
        )
    })
}

/// Serializable component failure details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentErrorView {
    /// Stable error code.
    pub code: String,
    /// User-safe message.
    pub message: String,
}

/// Serializable operation progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationView {
    /// Current phase label.
    pub phase: String,
    /// Progress from 0 through 100.
    pub percent: u8,
}

/// Serializable component status row for the interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentStatusView {
    /// Component name.
    pub component: String,
    /// Stable lowercase component state.
    pub state: String,
    /// Active version label, when one exists.
    pub active_version: Option<String>,
    /// Version label offered by the catalog, when available.
    pub available_version: Option<String>,
    /// Every installed version label, newest first.
    pub installed_versions: Vec<String>,
    /// Earlier installed version that verifies for rollback.
    pub rollback_version: Option<String>,
    /// Target key the projection used.
    pub target: String,
    /// Typed failure for a non-usable or failed-operation state.
    pub error: Option<ComponentErrorView>,
    /// In-flight operation, when one is running.
    pub operation: Option<OperationView>,
}

impl From<ComponentStatus> for ComponentStatusView {
    fn from(status: ComponentStatus) -> Self {
        Self {
            component: status.component,
            state: state_name(status.state).to_string(),
            active_version: status.active_version,
            available_version: status.available_version,
            installed_versions: status.installed_versions,
            rollback_version: status.rollback_version,
            target: status.target,
            error: status.error.map(|error| ComponentErrorView {
                code: error.code().to_string(),
                message: error.message().to_string(),
            }),
            operation: status.operation.map(|operation| OperationView {
                phase: operation.phase,
                percent: operation.percent,
            }),
        }
    }
}

fn state_name(state: ComponentState) -> &'static str {
    match state {
        ComponentState::Installed => "installed",
        ComponentState::NotInstalled => "not_installed",
        ComponentState::UpdateAvailable => "update_available",
        ComponentState::RollbackAvailable => "rollback_available",
        ComponentState::Tampered => "tampered",
        ComponentState::Unsupported => "unsupported",
        ComponentState::ManifestUnavailable => "manifest_unavailable",
    }
}

/// Project the component status rows.
pub fn kernel_status(state: &AppState) -> Result<Vec<ComponentStatusView>, AppError> {
    let statuses = runtime(state)?.statuses().map_err(app_error)?;
    Ok(statuses
        .into_iter()
        .map(ComponentStatusView::from)
        .collect())
}

/// Refresh the signed catalog from the configured manifest URL.
pub fn kernel_refresh(state: &AppState) -> Result<(), AppError> {
    runtime(state)?.refresh().map_err(app_error)
}

/// Install one component in the background.
pub fn kernel_install(state: &AppState, component: &str) -> Result<(), AppError> {
    runtime(state)?.install(component).map_err(app_error)
}

/// Update one component in the background.
pub fn kernel_update(state: &AppState, component: &str) -> Result<(), AppError> {
    runtime(state)?.update(component).map_err(app_error)
}

/// Roll one component back to an earlier installed version.
pub fn kernel_rollback(state: &AppState, component: &str) -> Result<(), AppError> {
    runtime(state)?.rollback(component).map_err(app_error)
}
