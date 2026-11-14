//! Behavior tests for the runtime component kernel behind the command surface:
//! catalog refresh, status projection, background install/update/rollback,
//! operation progress, busy rejection, and last-operation error surfacing.
//!
//! Every catalog is signed with an ephemeral Ed25519 key and served from a
//! loopback fixture server; artifacts come from a second loopback server. No
//! public network is contacted.

mod support;

use std::path::PathBuf;
use std::sync::Arc;
use std::thread::sleep;
use std::time::{Duration, Instant};

use ed25519_dalek::{Signer, SigningKey};
use oneget_lib::commands::kernel::{ComponentStatusView, KernelRuntime};
use oneget_lib::kernel::canonicalize;
use oneget_lib::kernel::catalog::{FileRole, Target, CATALOG_SCHEMA_VERSION};
use oneget_lib::kernel::status::{ComponentState, ComponentStatus};
use oneget_lib::kernel::store::{ComponentInstall, ComponentStore};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use support::kernel_server::{build_stored_zip, KernelServer, KernelServerConfig, ZipEntry};
use tempfile::{tempdir, TempDir};

const WAIT_TIMEOUT: Duration = Duration::from_secs(10);
const POLL_INTERVAL: Duration = Duration::from_millis(20);

fn wait_until<T>(timeout: Duration, context: &str, mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = check() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "timed out after {timeout:?} waiting for {context}"
        );
        sleep(POLL_INTERVAL);
    }
}

fn find_status<'a>(statuses: &'a [ComponentStatus], component: &str) -> &'a ComponentStatus {
    statuses
        .iter()
        .find(|status| status.component == component)
        .unwrap_or_else(|| panic!("status for {component}"))
}

// --- Signed catalog fixtures -------------------------------------------------

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn random_signing_key() -> SigningKey {
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).expect("os randomness is available for test key generation");
    SigningKey::from_bytes(&seed)
}

fn public_key_hex(signing_key: &SigningKey) -> String {
    hex_encode(signing_key.verifying_key().as_bytes())
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn sign_catalog(signing_key: &SigningKey, catalog: &Value) -> Vec<u8> {
    let canonical = canonicalize(catalog).expect("test catalog must canonicalize");
    let signature = signing_key.sign(canonical.as_bytes());
    let mut object = catalog
        .as_object()
        .expect("test catalog must be a JSON object")
        .clone();
    object.insert(
        "signature".to_string(),
        Value::String(hex_encode(&signature.to_bytes())),
    );
    serde_json::to_vec(&Value::Object(object)).expect("signed catalog must serialize")
}

#[derive(Clone)]
struct ArtifactSpec {
    url: String,
    sha256: String,
    size: u64,
    archive: &'static str,
    files: Vec<FileRole>,
}

impl ArtifactSpec {
    fn binary(name: &str, url: impl Into<String>, bytes: &[u8]) -> Self {
        Self {
            url: url.into(),
            sha256: sha256_hex(bytes),
            size: bytes.len() as u64,
            archive: "binary",
            files: vec![role(name, name)],
        }
    }

    fn zip(files: Vec<FileRole>, url: impl Into<String>, bytes: &[u8]) -> Self {
        Self {
            url: url.into(),
            sha256: sha256_hex(bytes),
            size: bytes.len() as u64,
            archive: "zip",
            files,
        }
    }

    fn with_sha256(mut self, sha256: impl Into<String>, size: u64) -> Self {
        self.sha256 = sha256.into();
        self.size = size;
        self
    }
}

struct ComponentSpec {
    name: &'static str,
    version: &'static str,
    sequence: u64,
    license: &'static str,
    artifact: ArtifactSpec,
}

fn role(role: &str, name: &str) -> FileRole {
    FileRole {
        role: role.to_string(),
        name: name.to_string(),
    }
}

fn ffmpeg_roles() -> Vec<FileRole> {
    vec![role("ffmpeg", "ffmpeg"), role("ffprobe", "ffprobe")]
}

fn component_json(spec: &ComponentSpec) -> Value {
    let current_key = Target::current().as_str();
    let mut artifacts = serde_json::Map::new();
    for key in ["macos-arm64", "macos-x64", "windows-x64"] {
        let (url, sha256) = if key == current_key {
            (spec.artifact.url.clone(), spec.artifact.sha256.clone())
        } else {
            (
                format!("https://cdn.example.com/{}/{key}", spec.name),
                "0".repeat(64),
            )
        };
        let files: Vec<Value> = spec
            .artifact
            .files
            .iter()
            .map(|file| json!({ "role": file.role, "name": file.name }))
            .collect();
        artifacts.insert(
            key.to_string(),
            json!({
                "url": url,
                "sha256": sha256,
                "size": spec.artifact.size,
                "archive": spec.artifact.archive,
                "files": files,
            }),
        );
    }
    json!({
        "name": spec.name,
        "version": spec.version,
        "sequence": spec.sequence,
        "license": spec.license,
        "artifacts": Value::Object(artifacts),
    })
}

fn catalog_value(components: &[ComponentSpec]) -> Value {
    let entries: Vec<Value> = components.iter().map(component_json).collect();
    json!({
        "schemaVersion": CATALOG_SCHEMA_VERSION,
        "generatedAt": "2026-11-13T00:00:00Z",
        "components": entries,
    })
}

fn signed_catalog(signing_key: &SigningKey, components: &[ComponentSpec]) -> Vec<u8> {
    sign_catalog(signing_key, &catalog_value(components))
}

const YT_DLP_BYTES: &[u8] = b"yt-dlp-command-fixture";
const FFMPEG_BYTES: &[u8] = b"ffmpeg-command-fixture";
const FFPROBE_BYTES: &[u8] = b"ffprobe-command-fixture";
const DENO_BYTES: &[u8] = b"deno-command-fixture";

fn ffmpeg_archive() -> Vec<u8> {
    build_stored_zip(&[
        ZipEntry::file("ffmpeg", FFMPEG_BYTES.to_vec()),
        ZipEntry::file("ffprobe", FFPROBE_BYTES.to_vec()),
    ])
}

/// A loopback server that only serves component artifacts.
fn artifact_server(delay: Option<Duration>) -> KernelServer {
    let mut config = KernelServerConfig::new(b"{}".to_vec())
        .artifact("yt-dlp", YT_DLP_BYTES.to_vec())
        .artifact("ffmpeg.zip", ffmpeg_archive())
        .artifact("deno", DENO_BYTES.to_vec());
    if let Some(delay) = delay {
        config = config.with_delay(delay);
    }
    KernelServer::start(config)
}

fn yt_dlp_spec(server: &KernelServer, version: &'static str, sequence: u64) -> ComponentSpec {
    ComponentSpec {
        name: "yt-dlp",
        version,
        sequence,
        license: "Unlicense",
        artifact: ArtifactSpec::binary("yt-dlp", server.artifact_url("yt-dlp"), YT_DLP_BYTES),
    }
}

fn ffmpeg_spec(server: &KernelServer, version: &'static str, sequence: u64) -> ComponentSpec {
    let archive = ffmpeg_archive();
    ComponentSpec {
        name: "ffmpeg",
        version,
        sequence,
        license: "LGPL-2.1-or-later",
        artifact: ArtifactSpec::zip(ffmpeg_roles(), server.artifact_url("ffmpeg.zip"), &archive),
    }
}

fn deno_spec(server: &KernelServer, version: &'static str, sequence: u64) -> ComponentSpec {
    ComponentSpec {
        name: "deno",
        version,
        sequence,
        license: "MIT",
        artifact: ArtifactSpec::binary("deno", server.artifact_url("deno"), DENO_BYTES),
    }
}

fn catalog_v1(server: &KernelServer, signing_key: &SigningKey) -> Vec<u8> {
    signed_catalog(
        signing_key,
        &[
            yt_dlp_spec(server, "2026.11.13", 1),
            ffmpeg_spec(server, "7.1", 2),
            deno_spec(server, "2.5.0", 3),
        ],
    )
}

fn catalog_v2(server: &KernelServer, signing_key: &SigningKey) -> Vec<u8> {
    signed_catalog(
        signing_key,
        &[
            yt_dlp_spec(server, "2026.11.14", 2),
            ffmpeg_spec(server, "7.1", 2),
            deno_spec(server, "2.5.0", 3),
        ],
    )
}

fn catalog_v2_bad_hash(server: &KernelServer, signing_key: &SigningKey) -> Vec<u8> {
    let mut yt_dlp = yt_dlp_spec(server, "2026.11.14", 2);
    yt_dlp.artifact = yt_dlp
        .artifact
        .with_sha256("0".repeat(64), YT_DLP_BYTES.len() as u64);
    signed_catalog(
        signing_key,
        &[
            yt_dlp,
            ffmpeg_spec(server, "7.1", 2),
            deno_spec(server, "2.5.0", 3),
        ],
    )
}

/// Shared temporary store, kernel root, and cache for one test.
struct RuntimeEnv {
    _root: TempDir,
    components_db: PathBuf,
    kernel_root: PathBuf,
    cache_dir: PathBuf,
    signing_key: SigningKey,
    public_key: String,
}

impl RuntimeEnv {
    fn new() -> Self {
        let root = tempdir().expect("tempdir");
        let signing_key = random_signing_key();
        let public_key = public_key_hex(&signing_key);
        Self {
            components_db: root.path().join("components.db"),
            kernel_root: root.path().join("kernel"),
            cache_dir: root.path().join("cache"),
            _root: root,
            signing_key,
            public_key,
        }
    }

    fn runtime(&self, manifest_url: Option<String>, trust: Option<String>) -> Arc<KernelRuntime> {
        KernelRuntime::new(
            self.components_db.clone(),
            self.kernel_root.clone(),
            self.cache_dir.clone(),
            trust,
            manifest_url,
            Target::current(),
            true,
        )
        .expect("kernel runtime constructs")
    }
}

// --- Tests ------------------------------------------------------------------

#[test]
fn kernel_runtime_status_install_update_rollback_end_to_end() {
    let env = RuntimeEnv::new();
    let artifacts = artifact_server(None);
    let catalog_v1 = catalog_v1(&artifacts, &env.signing_key);

    // Refresh sequence 1 and observe the available versions with no install.
    let catalog_server_v1 = KernelServer::start(KernelServerConfig::new(catalog_v1));
    let runtime_v1 = env.runtime(
        Some(catalog_server_v1.catalog_url()),
        Some(env.public_key.clone()),
    );
    runtime_v1.refresh().expect("sequence one refreshes");
    let statuses = runtime_v1.statuses().expect("statuses");
    for component in ["yt-dlp", "ffmpeg", "deno"] {
        let status = find_status(&statuses, component);
        assert_eq!(status.state, ComponentState::NotInstalled, "{component}");
        assert!(
            status.available_version.is_some(),
            "{component} available version"
        );
        assert!(
            status.active_version.is_none(),
            "{component} has no active version"
        );
    }

    // Install all three in the background and wait for the active records.
    for component in ["yt-dlp", "ffmpeg", "deno"] {
        runtime_v1.install(component).expect("install starts");
    }
    for component in ["yt-dlp", "ffmpeg", "deno"] {
        runtime_v1
            .wait_until_idle(component, WAIT_TIMEOUT)
            .unwrap_or_else(|error| panic!("install {component}: {error}"));
        wait_until(WAIT_TIMEOUT, "the installed state", || {
            let statuses = runtime_v1.statuses().ok()?;
            let status = statuses
                .iter()
                .find(|status| status.component == component)?;
            (status.state == ComponentState::Installed).then_some(())
        });
    }

    // A newer catalog reports an update instead of reinstalling.
    let catalog_v2 = catalog_v2(&artifacts, &env.signing_key);
    let catalog_server_v2 = KernelServer::start(KernelServerConfig::new(catalog_v2));
    let runtime_v2 = env.runtime(
        Some(catalog_server_v2.catalog_url()),
        Some(env.public_key.clone()),
    );
    runtime_v2.refresh().expect("sequence two refreshes");
    let statuses = runtime_v2.statuses().expect("statuses");
    let yt_dlp = find_status(&statuses, "yt-dlp");
    assert_eq!(yt_dlp.state, ComponentState::UpdateAvailable);
    assert_eq!(yt_dlp.available_version.as_deref(), Some("2026.11.14"));

    // Updating switches the active version and exposes the earlier one.
    runtime_v2.update("yt-dlp").expect("update starts");
    runtime_v2
        .wait_until_idle("yt-dlp", WAIT_TIMEOUT)
        .expect("update completes");
    wait_until(
        WAIT_TIMEOUT,
        "the updated and rollback-available state",
        || {
            let statuses = runtime_v2.statuses().ok()?;
            let status = statuses
                .iter()
                .find(|status| status.component == "yt-dlp")?;
            (status.active_version.as_deref() == Some("2026.11.14")
                && status.rollback_version.as_deref() == Some("2026.11.13")
                && status.state == ComponentState::RollbackAvailable)
                .then_some(())
        },
    );

    // Rollback must not use the network: stop the catalog server and assert the
    // artifact request count does not move.
    let artifact_requests = artifacts.request_count();
    drop(catalog_server_v2);
    runtime_v2
        .rollback("yt-dlp")
        .expect("rollback succeeds without a catalog server");
    wait_until(WAIT_TIMEOUT, "the rolled-back active version", || {
        let statuses = runtime_v2.statuses().ok()?;
        let status = statuses
            .iter()
            .find(|status| status.component == "yt-dlp")?;
        (status.active_version.as_deref() == Some("2026.11.13")).then_some(())
    });
    assert_eq!(
        artifacts.request_count(),
        artifact_requests,
        "rollback must not download an artifact"
    );
}

#[test]
fn kernel_runtime_reports_trust_and_manifest_unavailable() {
    let env = RuntimeEnv::new();
    let runtime = env.runtime(None, None);

    // No trust key rejects an install synchronously before any work.
    let error = runtime
        .install("yt-dlp")
        .expect_err("install without trust must fail");
    assert_eq!(error.code(), "COMPONENT_TRUST_UNCONFIGURED");

    // With no catalog the projection reports manifest unavailable.
    let statuses = runtime.statuses().expect("statuses without a catalog");
    for component in ["yt-dlp", "ffmpeg", "deno"] {
        let status = find_status(&statuses, component);
        assert_eq!(
            status.state,
            ComponentState::ManifestUnavailable,
            "{component}"
        );
        assert!(status.available_version.is_none(), "{component}");
    }

    // Refresh without trust fails before any request.
    let error = runtime
        .refresh()
        .expect_err("refresh without trust must fail");
    assert_eq!(error.code(), "COMPONENT_TRUST_UNCONFIGURED");

    // A transport failure maps to the stable manifest-unavailable code.
    let unreachable = env.runtime(
        Some("http://127.0.0.1:1/catalog.json".to_string()),
        Some(env.public_key.clone()),
    );
    let error = unreachable
        .refresh()
        .expect_err("an unreachable manifest must fail");
    assert_eq!(error.code(), "COMPONENT_MANIFEST_UNAVAILABLE");
}

#[test]
fn kernel_operation_is_background_and_bounded() {
    let env = RuntimeEnv::new();
    let artifacts = artifact_server(Some(Duration::from_millis(400)));
    let catalog = catalog_v1(&artifacts, &env.signing_key);
    let catalog_server = KernelServer::start(KernelServerConfig::new(catalog));
    let runtime = env.runtime(
        Some(catalog_server.catalog_url()),
        Some(env.public_key.clone()),
    );
    runtime.refresh().expect("refresh");

    // The install call itself returns quickly while the download continues.
    let started = Instant::now();
    runtime.install("yt-dlp").expect("install starts");
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_millis(250),
        "install must return without waiting for the download, took {elapsed:?}"
    );

    let operation = wait_until(WAIT_TIMEOUT, "the background operation", || {
        runtime
            .operation("yt-dlp")
            .filter(|operation| !operation.phase.is_empty())
    });
    assert!(
        operation.percent <= 100,
        "operation percent is bounded, got {}",
        operation.percent
    );

    // A second concurrent install for the same component is rejected as busy
    // before it can mutate state.
    let busy = runtime
        .install("yt-dlp")
        .expect_err("a concurrent install must be rejected");
    assert_eq!(busy.code(), "COMPONENT_BUSY");
    let statuses = runtime.statuses().expect("statuses while busy");
    assert!(
        find_status(&statuses, "yt-dlp").active_version.is_none(),
        "a rejected concurrent install must not record an active version"
    );

    runtime
        .wait_until_idle("yt-dlp", WAIT_TIMEOUT)
        .expect("the install completes");
    assert!(
        runtime.operation("yt-dlp").is_none(),
        "the operation clears after completion"
    );
    wait_until(WAIT_TIMEOUT, "the installed state", || {
        let statuses = runtime.statuses().ok()?;
        let status = statuses
            .iter()
            .find(|status| status.component == "yt-dlp")?;
        (status.state == ComponentState::Installed).then_some(())
    });
}

#[test]
fn kernel_command_error_surfaces_last_operation_error() {
    let env = RuntimeEnv::new();
    let artifacts = artifact_server(None);

    // Install a good sequence 1 first so the active version can be observed.
    let catalog_v1 = catalog_v1(&artifacts, &env.signing_key);
    let catalog_server_v1 = KernelServer::start(KernelServerConfig::new(catalog_v1));
    let runtime_v1 = env.runtime(
        Some(catalog_server_v1.catalog_url()),
        Some(env.public_key.clone()),
    );
    runtime_v1.refresh().expect("refresh sequence one");
    runtime_v1.install("yt-dlp").expect("install sequence one");
    runtime_v1
        .wait_until_idle("yt-dlp", WAIT_TIMEOUT)
        .expect("sequence one installs");

    // A bad artifact hash fails the update and keeps the active version.
    let catalog_v2 = catalog_v2_bad_hash(&artifacts, &env.signing_key);
    let catalog_server_v2 = KernelServer::start(KernelServerConfig::new(catalog_v2));
    let runtime_v2 = env.runtime(
        Some(catalog_server_v2.catalog_url()),
        Some(env.public_key.clone()),
    );
    runtime_v2.refresh().expect("refresh sequence two");
    runtime_v2.update("yt-dlp").expect("update starts");
    let error = runtime_v2
        .wait_until_idle("yt-dlp", WAIT_TIMEOUT)
        .expect_err("a bad artifact hash must fail the update");
    assert_eq!(error.code(), "COMPONENT_HASH");

    let last = runtime_v2
        .last_error("yt-dlp")
        .expect("the last operation error is retained");
    assert_eq!(last.code(), "COMPONENT_HASH");

    let statuses = runtime_v2.statuses().expect("statuses after failure");
    let yt_dlp = find_status(&statuses, "yt-dlp");
    assert_eq!(
        yt_dlp.active_version.as_deref(),
        Some("2026.11.13"),
        "a failed update must not change the active version"
    );
    let surfaced = yt_dlp
        .error
        .as_ref()
        .expect("the status overlays the failure");
    assert_eq!(surfaced.code(), "COMPONENT_HASH");
    assert!(
        find_status(&statuses, "ffmpeg").error.is_none(),
        "an unrelated component must not inherit the failure"
    );
    assert!(
        !env.kernel_root.join("yt-dlp/2-2026.11.14").exists(),
        "a failed update must leave no activated version directory"
    );
}

#[test]
fn kernel_status_serializes_snake_case_states() {
    fn wire_state(status: &ComponentStatus) -> String {
        let view = ComponentStatusView::from(status.clone());
        serde_json::to_value(&view).expect("status view serializes")["state"]
            .as_str()
            .expect("the state serializes as a string")
            .to_string()
    }

    let env = RuntimeEnv::new();

    // No catalog and nothing installed reports the manifest-unavailable state.
    let runtime = env.runtime(None, None);
    let statuses = runtime.statuses().expect("statuses without a catalog");
    for status in &statuses {
        assert_eq!(
            wire_state(status),
            "manifest_unavailable",
            "{}",
            status.component
        );
    }

    // A verified catalog with no active installations reports not_installed.
    let artifacts = artifact_server(None);
    let catalog = catalog_v1(&artifacts, &env.signing_key);
    let catalog_server = KernelServer::start(KernelServerConfig::new(catalog));
    let runtime = env.runtime(
        Some(catalog_server.catalog_url()),
        Some(env.public_key.clone()),
    );
    runtime.refresh().expect("sequence one refreshes");
    let statuses = runtime.statuses().expect("statuses after refresh");
    for status in &statuses {
        assert_eq!(wire_state(status), "not_installed", "{}", status.component);
    }

    // Verified active installations report the installed state.
    for component in ["yt-dlp", "ffmpeg", "deno"] {
        runtime.install(component).expect("install starts");
    }
    for component in ["yt-dlp", "ffmpeg", "deno"] {
        runtime
            .wait_until_idle(component, WAIT_TIMEOUT)
            .unwrap_or_else(|error| panic!("install {component}: {error}"));
    }
    let statuses = runtime.statuses().expect("statuses after install");
    for status in &statuses {
        assert_eq!(wire_state(status), "installed", "{}", status.component);
    }

    // A newer catalog reports update_available for the installed component.
    let catalog_v2 = catalog_v2(&artifacts, &env.signing_key);
    let catalog_server_v2 = KernelServer::start(KernelServerConfig::new(catalog_v2));
    let runtime_v2 = env.runtime(
        Some(catalog_server_v2.catalog_url()),
        Some(env.public_key.clone()),
    );
    runtime_v2.refresh().expect("sequence two refreshes");
    let statuses = runtime_v2.statuses().expect("statuses after sequence two");
    assert_eq!(
        wire_state(find_status(&statuses, "yt-dlp")),
        "update_available"
    );
    assert_eq!(wire_state(find_status(&statuses, "ffmpeg")), "installed");

    // Updating to the newer sequence exposes the earlier one for rollback.
    runtime_v2.update("yt-dlp").expect("update starts");
    runtime_v2
        .wait_until_idle("yt-dlp", WAIT_TIMEOUT)
        .expect("update completes");
    let statuses = runtime_v2.statuses().expect("statuses after update");
    assert_eq!(
        wire_state(find_status(&statuses, "yt-dlp")),
        "rollback_available"
    );

    // A corrupt active installation reports the tampered state.
    let tampered_file = env.kernel_root.join("yt-dlp/2-2026.11.14/yt-dlp");
    std::fs::write(&tampered_file, b"tampered-bytes").expect("installed file corrupts");
    let statuses = runtime_v2.statuses().expect("statuses after tamper");
    assert_eq!(wire_state(find_status(&statuses, "yt-dlp")), "tampered");

    // An install recorded for another target reports the unsupported state.
    let foreign_env = RuntimeEnv::new();
    let foreign_target = if Target::current() == Target::WindowsX64 {
        "macos-arm64"
    } else {
        "windows-x64"
    };
    let foreign = ComponentInstall {
        component: "yt-dlp".to_string(),
        version: "2026.11.13".to_string(),
        sequence: 1,
        target: foreign_target.to_string(),
        version_dir: "yt-dlp/1-2026.11.13".to_string(),
        files: Vec::new(),
        installed_at_ms: 1_760_000_000_000,
    };
    ComponentStore::open(&foreign_env.components_db)
        .expect("component store opens")
        .record_active(&foreign)
        .expect("foreign install records");
    let foreign_runtime = foreign_env.runtime(None, None);
    let statuses = foreign_runtime
        .statuses()
        .expect("statuses for the foreign install");
    assert_eq!(wire_state(find_status(&statuses, "yt-dlp")), "unsupported");
}
