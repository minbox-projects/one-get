//! Behavior tests for the component store, capability-scoped directory
//! operations, executable preparation, declared-file archive extraction, and
//! bounded artifact download verification.

mod support;

#[path = "support/proxy_server.rs"]
mod proxy_server;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ed25519_dalek::{Signer, SigningKey};
use oneget_lib::engine::http::ProxyConfig;
use oneget_lib::kernel::archive::{extract_declared_zip, ArchiveLimits, ExtractedFile};
use oneget_lib::kernel::canonicalize;
use oneget_lib::kernel::catalog::{
    verify_catalog, ArchiveType, Artifact, ComponentCatalog, ComponentEntry, FileRole, Target,
    CATALOG_SCHEMA_VERSION,
};
use oneget_lib::kernel::download::{download_artifact, download_artifact_with_proxy};
use oneget_lib::kernel::error::KernelError;
use oneget_lib::kernel::install::{InstallOutcome, Installer};
use oneget_lib::kernel::source::fetch_catalog_with_proxy;
use oneget_lib::kernel::status::{
    component_statuses, ComponentState, ComponentStatus, OperationRegistry,
};
use oneget_lib::kernel::store::{
    ComponentInstall, ComponentStore, InstalledFile, COMPONENTS_SCHEMA_VERSION,
};
use oneget_lib::kernel::tools::{ResolvedTools, ToolRequirement, ToolResolver};
use oneget_lib::platform::component::prepare_executable;
use oneget_lib::platform::files::CapDir;
use proxy_server::{ProxyCredentials, ProxyServer};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use support::kernel_server::{build_stored_zip, KernelServer, KernelServerConfig, ZipEntry};
use tempfile::tempdir;

fn installed_file(role: &str, name: &str, hash_byte: u8, size: u64) -> InstalledFile {
    InstalledFile {
        role: role.to_string(),
        name: name.to_string(),
        sha256: format!("{hash_byte:02x}").repeat(32),
        size,
    }
}

fn install(component: &str, version: &str, sequence: u64) -> ComponentInstall {
    ComponentInstall {
        component: component.to_string(),
        version: version.to_string(),
        sequence,
        target: "macos-arm64".to_string(),
        version_dir: format!("{component}/{sequence}"),
        files: vec![
            installed_file("ffmpeg", "ffmpeg", 0xab, 128),
            installed_file("ffprobe", "ffprobe", 0xcd, 256),
        ],
        installed_at_ms: 1_760_000_000_000 + sequence as i64,
    }
}

fn file_tuples(install: &ComponentInstall) -> Vec<(String, String, String, u64)> {
    install
        .files
        .iter()
        .map(|file| {
            (
                file.role.clone(),
                file.name.clone(),
                file.sha256.clone(),
                file.size,
            )
        })
        .collect()
}

fn ffmpeg_roles() -> Vec<FileRole> {
    vec![
        FileRole {
            role: "ffmpeg".to_string(),
            name: "ffmpeg".to_string(),
        },
        FileRole {
            role: "ffprobe".to_string(),
            name: "ffprobe".to_string(),
        },
    ]
}

fn role(role: &str, name: &str) -> FileRole {
    FileRole {
        role: role.to_string(),
        name: name.to_string(),
    }
}

fn assert_extracted(files: &[ExtractedFile], name: &str, data: &[u8]) {
    let file = files
        .iter()
        .find(|file| file.name == name)
        .expect("declared file must be recorded");
    assert_eq!(file.role, name, "declared role must round-trip");
    assert_eq!(file.size, data.len() as u64, "recorded size must match");
    assert_eq!(
        file.sha256,
        hex::encode(Sha256::digest(data)),
        "recorded sha256 must match the extracted bytes"
    );
}

#[test]
fn component_store_keeps_one_active_record_and_refuses_newer_schema() {
    let root = tempdir().expect("tempdir");
    let db_path = root.path().join("components.db");
    let store = ComponentStore::open(&db_path).expect("open component store");

    assert_eq!(COMPONENTS_SCHEMA_VERSION, 1);
    assert_eq!(
        store.schema_version().expect("read schema version"),
        COMPONENTS_SCHEMA_VERSION
    );

    let first = install("ffmpeg", "7.0", 1);
    let second = install("ffmpeg", "7.1", 2);
    store.record_active(&first).expect("record sequence one");
    store.record_active(&second).expect("record sequence two");

    let active = store
        .active("ffmpeg")
        .expect("query active component")
        .expect("one active record");
    assert_eq!(active.component, "ffmpeg");
    assert_eq!(active.version, second.version);
    assert_eq!(active.sequence, 2);
    assert_eq!(active.target, second.target);
    assert_eq!(active.version_dir, second.version_dir);
    assert_eq!(active.installed_at_ms, second.installed_at_ms);
    assert_eq!(file_tuples(&active), file_tuples(&second));

    let sequences: Vec<u64> = store
        .installed("ffmpeg")
        .expect("list installed records")
        .iter()
        .map(|record| record.sequence)
        .collect();
    assert_eq!(
        sequences,
        vec![2, 1],
        "installed records descend by sequence"
    );
    assert!(store
        .installed("deno")
        .expect("list absent component")
        .is_empty());

    let newer_path = root.path().join("newer.db");
    {
        let conn = rusqlite::Connection::open(&newer_path).expect("create newer database");
        conn.execute_batch(
            "CREATE TABLE schema_meta (key TEXT PRIMARY KEY NOT NULL, value TEXT NOT NULL);
             INSERT INTO schema_meta (key, value) VALUES ('schema_version', '2');",
        )
        .expect("write newer schema metadata");
    }
    let Err(error) = ComponentStore::open(&newer_path) else {
        panic!("a newer schema must be refused");
    };
    assert_eq!(error.code(), "COMPONENT_SCHEMA_UNSUPPORTED");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = std::fs::metadata(&db_path)
            .expect("component database metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "components.db must be owner-only");
    }
}

#[tokio::test]
async fn capability_directory_operations_stay_inside_the_root() {
    let root = tempdir().expect("tempdir");
    let cap = CapDir::open(root.path().join("cap")).expect("open capability root");

    cap.create_dir_all("a/b")
        .expect("create nested directories");
    cap.write("a/b/file.txt", b"hello")
        .await
        .expect("write nested file");
    assert_eq!(
        cap.read("a/b/file.txt").await.expect("read nested file"),
        b"hello"
    );

    cap.create_dir("c").expect("create directory");
    cap.rename("a/b/file.txt", "c/moved.txt")
        .expect("rename into another directory");
    assert_eq!(
        cap.read("c/moved.txt").await.expect("read moved file"),
        b"hello"
    );
    assert!(!cap.exists("a/b/file.txt"), "rename must move the source");

    cap.write("c/existing.txt", b"keep")
        .await
        .expect("write existing destination");
    let rename_error = cap
        .rename("c/moved.txt", "c/existing.txt")
        .expect_err("rename onto an existing path must fail");
    assert!(!rename_error.code.is_empty());
    assert_eq!(
        cap.read("c/existing.txt")
            .await
            .expect("read existing destination"),
        b"keep",
        "a refused rename must not replace the destination"
    );

    cap.create_dir("empty").expect("create empty directory");
    cap.remove_dir("empty").expect("remove empty directory");
    assert!(!cap.exists("empty"), "remove_dir must delete the directory");

    for escape in ["../escape", "/absolute", "a/../../escape"] {
        let error = cap
            .create_dir(escape)
            .expect_err("escaping directory path must be refused");
        assert!(!error.code.is_empty());
    }
}

#[tokio::test]
async fn prepare_executable_sets_unix_mode_and_is_idempotent() {
    let root = tempdir().expect("tempdir");
    let cap = CapDir::open(root.path().join("cap")).expect("open capability root");

    cap.create_dir_all("bin").expect("create bin directory");
    cap.write("bin/tool", b"#!/bin/sh\nexit 0\n")
        .await
        .expect("write tool");

    prepare_executable(&cap, "bin/tool").expect("prepare executable");
    prepare_executable(&cap, "bin/tool").expect("second preparation is idempotent");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = std::fs::metadata(root.path().join("cap/bin/tool"))
            .expect("executable metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755, "prepared file must be executable");
    }

    let error =
        prepare_executable(&cap, "bin/missing").expect_err("preparing a missing file must fail");
    assert!(!error.code.is_empty());
}

#[test]
fn archive_extracts_only_declared_files_and_records_hashes() {
    let limits = ArchiveLimits::default();
    assert_eq!(limits.max_entries, 32);
    assert_eq!(limits.max_file_bytes, 256 * 1024 * 1024);
    assert_eq!(limits.max_total_bytes, 512 * 1024 * 1024);
    assert_eq!(limits.max_compression_ratio, 200);

    let ffmpeg_bytes: &[u8] = b"ffmpeg-binary-fixture";
    let ffprobe_bytes: &[u8] = b"ffprobe-binary-fixture";
    let archive = build_stored_zip(&[
        ZipEntry::file("ffmpeg", ffmpeg_bytes),
        ZipEntry::file("ffprobe", ffprobe_bytes),
    ]);
    let root = tempdir().expect("tempdir");
    let destination = CapDir::open(root.path().join("dest")).expect("open destination");

    let extracted = extract_declared_zip(&archive, &ffmpeg_roles(), &destination, &limits)
        .expect("declared files must extract");

    assert_eq!(extracted.len(), 2);
    assert_extracted(&extracted, "ffmpeg", ffmpeg_bytes);
    assert_extracted(&extracted, "ffprobe", ffprobe_bytes);

    assert_eq!(
        std::fs::read(destination.root().join("ffmpeg")).expect("read extracted ffmpeg"),
        ffmpeg_bytes
    );
    let entries = std::fs::read_dir(destination.root())
        .expect("read destination")
        .count();
    assert_eq!(entries, 2, "only declared files are written");
}

fn expect_rejected(
    entries: &[ZipEntry],
    declared: &[FileRole],
    limits: &ArchiveLimits,
) -> KernelError {
    let root = tempdir().expect("tempdir");
    let destination = CapDir::open(root.path().join("dest")).expect("open destination");
    let archive = build_stored_zip(entries);

    let error = extract_declared_zip(&archive, declared, &destination, limits)
        .expect_err("archive must be rejected");
    assert!(
        !error.code().is_empty(),
        "rejection must carry a typed code"
    );

    let beside_destination = std::fs::read_dir(root.path())
        .expect("read capability root")
        .count();
    assert_eq!(
        beside_destination, 1,
        "nothing may be written beside the destination directory"
    );
    let extracted = std::fs::read_dir(destination.root())
        .expect("read destination")
        .count();
    assert_eq!(
        extracted, 0,
        "a rejected archive must leave no extracted file"
    );
    error
}

#[test]
fn archive_rejects_unsafe_and_bomb_entries() {
    let defaults = || ArchiveLimits::default();

    expect_rejected(
        &[ZipEntry::file("../escape", b"escape")],
        &[role("ffmpeg", "../escape")],
        &defaults(),
    );
    expect_rejected(
        &[ZipEntry::file("/absolute-escape", b"escape")],
        &[role("ffmpeg", "/absolute-escape")],
        &defaults(),
    );
    expect_rejected(
        &[ZipEntry::file("dir\\ffmpeg", b"escape")],
        &[role("ffmpeg", "dir\\ffmpeg")],
        &defaults(),
    );
    expect_rejected(
        &[ZipEntry::symlink("ffmpeg", "/etc/passwd")],
        &[role("ffmpeg", "ffmpeg")],
        &defaults(),
    );
    expect_rejected(
        &[
            ZipEntry::file("ffmpeg", b"first"),
            ZipEntry::file("ffmpeg", b"second"),
        ],
        &[role("ffmpeg", "ffmpeg")],
        &defaults(),
    );
    expect_rejected(
        &[ZipEntry::file("ffmpeg", b"only-ffmpeg")],
        &ffmpeg_roles(),
        &defaults(),
    );
    // A declared file plus an additional undeclared regular file must be rejected.
    expect_rejected(
        &[
            ZipEntry::file("ffmpeg", b"declared"),
            ZipEntry::file("notes.txt", b"undeclared"),
        ],
        &[role("ffmpeg", "ffmpeg")],
        &defaults(),
    );
    expect_rejected(
        &[
            ZipEntry::file("ffmpeg", b"one"),
            ZipEntry::file("ffprobe", b"two"),
        ],
        &ffmpeg_roles(),
        &ArchiveLimits {
            max_entries: 1,
            ..ArchiveLimits::default()
        },
    );
    expect_rejected(
        &[ZipEntry::file("ffmpeg", b"0123456789")],
        &[role("ffmpeg", "ffmpeg")],
        &ArchiveLimits {
            max_file_bytes: 4,
            ..ArchiveLimits::default()
        },
    );
    expect_rejected(
        &[
            ZipEntry::file("ffmpeg", b"four"),
            ZipEntry::file("ffprobe", b"four"),
        ],
        &ffmpeg_roles(),
        &ArchiveLimits {
            max_total_bytes: 5,
            ..ArchiveLimits::default()
        },
    );
    expect_rejected(
        &[ZipEntry::file("ffmpeg", b"0123456789abcdef").with_declared_uncompressed_size(4096)],
        &[role("ffmpeg", "ffmpeg")],
        &defaults(),
    );
}

#[tokio::test]
async fn artifact_download_verifies_hash_size_and_url_policy() {
    let bytes: &[u8] = b"canonical-component-artifact";
    let digest = hex::encode(Sha256::digest(bytes));
    let server = KernelServer::start(
        KernelServerConfig::new(b"{}".to_vec()).artifact("ffmpeg.bin", bytes.to_vec()),
    );
    let url = server.artifact_url("ffmpeg.bin");
    let timeout = Duration::from_secs(5);
    let size = bytes.len() as u64;

    let downloaded = download_artifact(&url, &digest, size, true, timeout)
        .await
        .expect("a matching artifact must download");
    assert_eq!(downloaded, bytes);

    let error = download_artifact(&url, &"0".repeat(64), size, true, timeout)
        .await
        .expect_err("a wrong expected hash must fail");
    assert_eq!(error.code(), "COMPONENT_HASH");

    let error = download_artifact(&url, &digest, size + 8, true, timeout)
        .await
        .expect_err("a truncated artifact must fail");
    assert_eq!(error.code(), "COMPONENT_HASH");

    let error = download_artifact(&url, &digest, size - 1, true, timeout)
        .await
        .expect_err("an oversized artifact must fail");
    assert_eq!(error.code(), "COMPONENT_HASH");

    let requests_before_policy = server.request_count();
    let error = download_artifact(&url, &digest, size, false, timeout)
        .await
        .expect_err("loopback HTTP without the test opt-in must be rejected");
    assert!(!error.code().is_empty());
    assert_eq!(
        server.request_count(),
        requests_before_policy,
        "URL policy must reject before any request"
    );

    let error = download_artifact(
        "http://198.51.100.10/ffmpeg.bin",
        &digest,
        size,
        true,
        timeout,
    )
    .await
    .expect_err("non-loopback plain HTTP must be rejected");
    assert!(!error.code().is_empty());
}

// ---------------------------------------------------------------------------
// Signed-catalog fixtures for the installer behavior tests.
// ---------------------------------------------------------------------------

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

/// Sign a catalog object exactly like the kernel signs JSON: a hex signature
/// over the canonical form of the object without `signature`.
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

fn verified_catalog(bytes: &[u8], public_key: &str) -> ComponentCatalog {
    verify_catalog(bytes, Some(public_key), Target::current(), true)
        .expect("signed catalog must verify")
}

fn unused_ffmpeg_spec() -> ComponentSpec {
    ComponentSpec {
        name: "ffmpeg",
        version: "7.1",
        sequence: 5,
        license: "LGPL-2.1-or-later",
        artifact: ArtifactSpec::zip(
            ffmpeg_roles(),
            "https://cdn.example.com/ffmpeg.zip",
            b"unused-ffmpeg-archive",
        ),
    }
}

fn unused_deno_spec() -> ComponentSpec {
    ComponentSpec {
        name: "deno",
        version: "2.5.0",
        sequence: 6,
        license: "MIT",
        artifact: ArtifactSpec::binary(
            "deno",
            "https://cdn.example.com/deno",
            b"unused-deno-binary",
        ),
    }
}

/// A catalog type that declares `yt-dlp` only for a non-current target, which
/// catalog verification itself would reject; the installer must refuse it too.
fn catalog_without_current_target() -> ComponentCatalog {
    let current = Target::current();
    let other = [Target::MacosArm64, Target::MacosX64, Target::WindowsX64]
        .into_iter()
        .find(|target| *target != current)
        .expect("a non-current target exists");
    let mut artifacts = BTreeMap::new();
    artifacts.insert(
        other,
        Artifact {
            url: "https://cdn.example.com/yt-dlp/other".to_string(),
            sha256: "0".repeat(64),
            size: 1,
            archive: ArchiveType::Binary,
            files: vec![role("yt-dlp", "yt-dlp")],
        },
    );
    let mut components = BTreeMap::new();
    components.insert(
        "yt-dlp".to_string(),
        ComponentEntry {
            name: "yt-dlp".to_string(),
            display_version: "2026.11.13".to_string(),
            sequence: 1,
            license: "Unlicense".to_string(),
            artifacts,
        },
    );
    ComponentCatalog {
        schema_version: CATALOG_SCHEMA_VERSION,
        generated_at: "2026-11-13T00:00:00Z".to_string(),
        components,
    }
}

fn verify_installed_files(kernel_root: &CapDir, install: &ComponentInstall) {
    let version_root = kernel_root.root().join(&install.version_dir);
    assert!(
        version_root.is_dir(),
        "version directory {} must exist",
        version_root.display()
    );
    for file in &install.files {
        let path = version_root.join(&file.name);
        let data = std::fs::read(&path)
            .unwrap_or_else(|error| panic!("read installed file {}: {error}", path.display()));
        assert_eq!(
            sha256_hex(&data),
            file.sha256,
            "recorded hash must match {}",
            file.name
        );
        assert_eq!(
            data.len() as u64,
            file.size,
            "recorded size must match {}",
            file.name
        );

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            let mode = std::fs::metadata(&path)
                .expect("installed file metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o755, "{} must be executable", file.name);
        }
    }
}

fn assert_no_temporary_entries(root: &Path) {
    fn walk(directory: &Path, root: &Path) {
        for entry in std::fs::read_dir(directory).expect("read kernel root directory") {
            let entry = entry.expect("read kernel root entry");
            let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
            for marker in ["tmp", "temp", "staging", "partial", "download"] {
                assert!(
                    !name.contains(marker),
                    "temporary entry {} remains under {}",
                    entry.path().display(),
                    root.display()
                );
            }
            if entry.file_type().expect("kernel entry file type").is_dir() {
                walk(&entry.path(), root);
            }
        }
    }

    walk(root, root);
}

#[tokio::test]
async fn install_activates_version_directory_and_records_hashes() {
    let signing_key = random_signing_key();
    let public_key = public_key_hex(&signing_key);
    let yt_dlp_bytes: &[u8] = b"yt-dlp-binary-fixture";
    let ffmpeg_zip = build_stored_zip(&[
        ZipEntry::file("ffmpeg", b"ffmpeg-binary-fixture".as_slice()),
        ZipEntry::file("ffprobe", b"ffprobe-binary-fixture".as_slice()),
    ]);

    let server = KernelServer::start(
        KernelServerConfig::new(b"{}".to_vec())
            .artifact("yt-dlp", yt_dlp_bytes.to_vec())
            .artifact("ffmpeg.zip", ffmpeg_zip.clone()),
    );

    let components = [
        ComponentSpec {
            name: "yt-dlp",
            version: "2026.11.13",
            sequence: 4,
            license: "Unlicense",
            artifact: ArtifactSpec::binary("yt-dlp", server.artifact_url("yt-dlp"), yt_dlp_bytes),
        },
        ComponentSpec {
            name: "ffmpeg",
            version: "7.1",
            sequence: 5,
            license: "LGPL-2.1-or-later",
            artifact: ArtifactSpec::zip(
                ffmpeg_roles(),
                server.artifact_url("ffmpeg.zip"),
                &ffmpeg_zip,
            ),
        },
        unused_deno_spec(),
    ];
    let catalog = verified_catalog(&signed_catalog(&signing_key, &components), &public_key);

    let root = tempdir().expect("tempdir");
    let db_path = root.path().join("components.db");
    let kernel_root = CapDir::open(root.path().join("kernel")).expect("open kernel root");
    let store = ComponentStore::open(&db_path).expect("open component store");
    let reader = ComponentStore::open(&db_path).expect("open reader connection");
    let operations = Arc::new(OperationRegistry::new());
    let mut installer = Installer::new(
        store,
        kernel_root.clone(),
        Target::current(),
        Some(public_key.clone()),
        true,
    );
    installer.set_operations(Arc::clone(&operations));

    let yt_dlp = installer
        .install(&catalog, "yt-dlp")
        .await
        .expect("install yt-dlp");
    assert_eq!(yt_dlp.component, "yt-dlp");
    assert_eq!(yt_dlp.sequence, 4);
    assert_eq!(yt_dlp.version, "2026.11.13");
    assert_eq!(yt_dlp.target, Target::current().as_str());
    assert_eq!(yt_dlp.version_dir, "yt-dlp/4-2026.11.13");
    assert!(
        operations.get("yt-dlp").is_none(),
        "the operation state must clear after a successful install"
    );

    let ffmpeg = installer
        .install(&catalog, "ffmpeg")
        .await
        .expect("install ffmpeg");
    assert_eq!(ffmpeg.component, "ffmpeg");
    assert_eq!(ffmpeg.sequence, 5);
    assert_eq!(ffmpeg.version, "7.1");
    assert_eq!(ffmpeg.target, Target::current().as_str());
    assert_eq!(ffmpeg.version_dir, "ffmpeg/5-7.1");
    assert_eq!(ffmpeg.files.len(), 2);
    assert!(
        operations.get("ffmpeg").is_none(),
        "the operation state must clear after a successful install"
    );

    let active_yt_dlp = reader
        .active("yt-dlp")
        .expect("read active yt-dlp")
        .expect("yt-dlp must be active");
    assert_eq!(file_tuples(&active_yt_dlp), file_tuples(&yt_dlp));
    assert_eq!(active_yt_dlp.target, Target::current().as_str());

    let active_ffmpeg = reader
        .active("ffmpeg")
        .expect("read active ffmpeg")
        .expect("ffmpeg must be active");
    assert_eq!(file_tuples(&active_ffmpeg), file_tuples(&ffmpeg));
    assert_eq!(active_ffmpeg.target, Target::current().as_str());

    verify_installed_files(&kernel_root, &yt_dlp);
    verify_installed_files(&kernel_root, &ffmpeg);
}

#[tokio::test]
async fn install_failure_preserves_previous_active_version_and_cleans_temporary_files() {
    let signing_key = random_signing_key();
    let public_key = public_key_hex(&signing_key);
    let first_bytes: &[u8] = b"yt-dlp-sequence-one";
    let second_served: &[u8] = b"yt-dlp-sequence-two";

    let server = KernelServer::start(
        KernelServerConfig::new(b"{}".to_vec())
            .artifact("yt-dlp-v1", first_bytes.to_vec())
            .artifact("yt-dlp-v2", second_served.to_vec()),
    );

    let first_components = [
        ComponentSpec {
            name: "yt-dlp",
            version: "2026.11.13",
            sequence: 1,
            license: "Unlicense",
            artifact: ArtifactSpec::binary("yt-dlp", server.artifact_url("yt-dlp-v1"), first_bytes),
        },
        unused_ffmpeg_spec(),
        unused_deno_spec(),
    ];
    let second_components = [
        ComponentSpec {
            name: "yt-dlp",
            version: "2026.11.14",
            sequence: 2,
            license: "Unlicense",
            artifact: ArtifactSpec::binary(
                "yt-dlp",
                server.artifact_url("yt-dlp-v2"),
                second_served,
            )
            .with_sha256("0".repeat(64), second_served.len() as u64),
        },
        unused_ffmpeg_spec(),
        unused_deno_spec(),
    ];

    let first_catalog = verified_catalog(
        &signed_catalog(&signing_key, &first_components),
        &public_key,
    );
    let second_catalog = verified_catalog(
        &signed_catalog(&signing_key, &second_components),
        &public_key,
    );

    let root = tempdir().expect("tempdir");
    let db_path = root.path().join("components.db");
    let kernel_root = CapDir::open(root.path().join("kernel")).expect("open kernel root");
    let store = ComponentStore::open(&db_path).expect("open component store");
    let reader = ComponentStore::open(&db_path).expect("open reader connection");
    let installer = Installer::new(
        store,
        kernel_root.clone(),
        Target::current(),
        Some(public_key),
        true,
    );

    let first = installer
        .install(&first_catalog, "yt-dlp")
        .await
        .expect("install the first sequence");
    assert_eq!(first.sequence, 1);

    let error = installer
        .install(&second_catalog, "yt-dlp")
        .await
        .expect_err("bytes that do not match the declared hash must fail");
    assert_eq!(error.code(), "COMPONENT_HASH");

    let active = reader
        .active("yt-dlp")
        .expect("read active yt-dlp")
        .expect("the first sequence must stay active");
    assert_eq!(active.sequence, 1);
    assert_eq!(file_tuples(&active), file_tuples(&first));
    assert!(
        !kernel_root.root().join("yt-dlp/2-2026.11.14").exists(),
        "a failed install must not activate the second version"
    );
    assert_no_temporary_entries(kernel_root.root());
}

#[tokio::test]
async fn update_requires_a_strictly_newer_sequence() {
    let signing_key = random_signing_key();
    let public_key = public_key_hex(&signing_key);
    let v1_bytes: &[u8] = b"yt-dlp-update-one";
    let v2_bytes: &[u8] = b"yt-dlp-update-two";

    let server = KernelServer::start(
        KernelServerConfig::new(b"{}".to_vec())
            .artifact("yt-dlp-v1", v1_bytes.to_vec())
            .artifact("yt-dlp-v2", v2_bytes.to_vec()),
    );

    let v1_components = [
        ComponentSpec {
            name: "yt-dlp",
            version: "2026.11.13",
            sequence: 1,
            license: "Unlicense",
            artifact: ArtifactSpec::binary("yt-dlp", server.artifact_url("yt-dlp-v1"), v1_bytes),
        },
        unused_ffmpeg_spec(),
        unused_deno_spec(),
    ];
    let v2_components = [
        ComponentSpec {
            name: "yt-dlp",
            version: "2026.11.14",
            sequence: 2,
            license: "Unlicense",
            artifact: ArtifactSpec::binary("yt-dlp", server.artifact_url("yt-dlp-v2"), v2_bytes),
        },
        unused_ffmpeg_spec(),
        unused_deno_spec(),
    ];
    let catalog_v1 = verified_catalog(&signed_catalog(&signing_key, &v1_components), &public_key);
    let catalog_v2 = verified_catalog(&signed_catalog(&signing_key, &v2_components), &public_key);

    let root = tempdir().expect("tempdir");
    let db_path = root.path().join("components.db");
    let kernel_root = CapDir::open(root.path().join("kernel")).expect("open kernel root");
    let store = ComponentStore::open(&db_path).expect("open component store");
    let reader = ComponentStore::open(&db_path).expect("open reader connection");
    let installer = Installer::new(
        store,
        kernel_root,
        Target::current(),
        Some(public_key),
        true,
    );

    let first = installer
        .install(&catalog_v1, "yt-dlp")
        .await
        .expect("install sequence one");
    assert_eq!(first.sequence, 1);

    let requests_after_install = server.request_count();
    match installer
        .update(&catalog_v1, "yt-dlp")
        .await
        .expect("an equal sequence is not an error")
    {
        InstallOutcome::UpToDate(install) => {
            assert_eq!(install.sequence, 1);
            assert_eq!(install.version_dir, "yt-dlp/1-2026.11.13");
        }
        _ => panic!("an equal sequence must report UpToDate"),
    }
    assert_eq!(
        server.request_count(),
        requests_after_install,
        "an equal sequence must not download"
    );

    match installer
        .update(&catalog_v2, "yt-dlp")
        .await
        .expect("a newer sequence must install")
    {
        InstallOutcome::Installed(install) => {
            assert_eq!(install.sequence, 2);
            assert_eq!(install.version_dir, "yt-dlp/2-2026.11.14");
        }
        _ => panic!("a newer sequence must report Installed"),
    }

    let active = reader
        .active("yt-dlp")
        .expect("read active yt-dlp")
        .expect("yt-dlp must be active");
    assert_eq!(active.sequence, 2);

    let requests_before_downgrade = server.request_count();
    let downgrade = installer
        .update(&catalog_v1, "yt-dlp")
        .await
        .expect_err("a lower sequence must be refused");
    assert_eq!(downgrade.code(), "COMPONENT_VERSION_MISMATCH");
    assert_eq!(
        server.request_count(),
        requests_before_downgrade,
        "a refused downgrade must not download"
    );
    let active = reader
        .active("yt-dlp")
        .expect("read active yt-dlp")
        .expect("yt-dlp must stay active");
    assert_eq!(active.sequence, 2, "a refused downgrade must not switch");

    let sequences: Vec<u64> = reader
        .installed("yt-dlp")
        .expect("list installed yt-dlp")
        .iter()
        .map(|record| record.sequence)
        .collect();
    assert_eq!(sequences, vec![2, 1], "both versions must remain installed");
}

#[tokio::test]
async fn rollback_switches_to_an_installed_earlier_version_without_network() {
    let signing_key = random_signing_key();
    let public_key = public_key_hex(&signing_key);
    let v1_bytes: &[u8] = b"yt-dlp-rollback-one";
    let v2_bytes: &[u8] = b"yt-dlp-rollback-two";
    let deno_bytes: &[u8] = b"deno-rollback-binary";

    let server = KernelServer::start(
        KernelServerConfig::new(b"{}".to_vec())
            .artifact("yt-dlp-v1", v1_bytes.to_vec())
            .artifact("yt-dlp-v2", v2_bytes.to_vec())
            .artifact("deno", deno_bytes.to_vec()),
    );
    let deno_spec = || ComponentSpec {
        name: "deno",
        version: "2.5.0",
        sequence: 6,
        license: "MIT",
        artifact: ArtifactSpec::binary("deno", server.artifact_url("deno"), deno_bytes),
    };

    let v1_components = [
        ComponentSpec {
            name: "yt-dlp",
            version: "2026.11.13",
            sequence: 1,
            license: "Unlicense",
            artifact: ArtifactSpec::binary("yt-dlp", server.artifact_url("yt-dlp-v1"), v1_bytes),
        },
        unused_ffmpeg_spec(),
        deno_spec(),
    ];
    let v2_components = [
        ComponentSpec {
            name: "yt-dlp",
            version: "2026.11.14",
            sequence: 2,
            license: "Unlicense",
            artifact: ArtifactSpec::binary("yt-dlp", server.artifact_url("yt-dlp-v2"), v2_bytes),
        },
        unused_ffmpeg_spec(),
        deno_spec(),
    ];
    let catalog_v1 = verified_catalog(&signed_catalog(&signing_key, &v1_components), &public_key);
    let catalog_v2 = verified_catalog(&signed_catalog(&signing_key, &v2_components), &public_key);

    let root = tempdir().expect("tempdir");
    let db_path = root.path().join("components.db");
    let kernel_root = CapDir::open(root.path().join("kernel")).expect("open kernel root");
    let store = ComponentStore::open(&db_path).expect("open component store");
    let installer = Installer::new(
        store,
        kernel_root.clone(),
        Target::current(),
        Some(public_key),
        true,
    );

    installer
        .install(&catalog_v1, "yt-dlp")
        .await
        .expect("install sequence one");
    installer
        .update(&catalog_v2, "yt-dlp")
        .await
        .expect("update to sequence two");
    installer
        .install(&catalog_v2, "deno")
        .await
        .expect("install deno once");

    // One request per download: sequence one, sequence two, and deno.
    let requests_before_rollback = server.request_count();
    assert_eq!(
        requests_before_rollback, 3,
        "each successful install must make exactly one artifact request"
    );
    // Shut the fixture down entirely; a rollback that attempted any network
    // request would now fail instead of switching the active pointer.
    drop(server);

    let rolled = installer
        .rollback("yt-dlp")
        .await
        .expect("rollback must use local records only");
    assert_eq!(rolled.sequence, 1);
    assert_eq!(rolled.version_dir, "yt-dlp/1-2026.11.13");
    assert_eq!(rolled.target, Target::current().as_str());
    verify_installed_files(&kernel_root, &rolled);

    let reader = ComponentStore::open(&db_path).expect("open reader connection");
    let active = reader
        .active("yt-dlp")
        .expect("read active yt-dlp")
        .expect("yt-dlp must be active");
    assert_eq!(
        active.sequence, 1,
        "rollback must switch the active pointer"
    );
    let sequences: Vec<u64> = reader
        .installed("yt-dlp")
        .expect("list installed yt-dlp")
        .iter()
        .map(|record| record.sequence)
        .collect();
    assert_eq!(
        sequences,
        vec![2, 1],
        "the updated version must remain installed after rollback"
    );

    let deno_before = reader
        .active("deno")
        .expect("read active deno")
        .expect("deno must be active");
    let no_earlier = installer
        .rollback("deno")
        .await
        .expect_err("a component with one version cannot roll back");
    assert!(
        !no_earlier.code().is_empty(),
        "the refusal must carry a typed code"
    );
    let deno_after = reader
        .active("deno")
        .expect("read active deno")
        .expect("deno must stay active");
    assert_eq!(
        file_tuples(&deno_after),
        file_tuples(&deno_before),
        "a refused rollback must not change the active version"
    );
}

#[tokio::test]
async fn operation_registry_reports_bounded_phase_and_percent() {
    let registry = OperationRegistry::new();

    registry.begin("yt-dlp");
    let state = registry
        .get("yt-dlp")
        .expect("begin must create an operation state");
    assert!(state.phase.is_empty(), "the initial phase must be empty");
    assert_eq!(state.percent, 0);

    registry.update("yt-dlp", "downloading", 42);
    let state = registry.get("yt-dlp").expect("updated state must exist");
    assert_eq!(state.phase, "downloading");
    assert_eq!(state.percent, 42);

    registry.update("yt-dlp", "extracting", 150);
    let state = registry.get("yt-dlp").expect("clamped state must exist");
    assert_eq!(state.phase, "extracting");
    assert_eq!(state.percent, 100, "percent must clamp to 100");

    registry.begin("ffmpeg");
    registry.update("ffmpeg", "verifying", 10);
    registry.finish("yt-dlp");

    assert!(
        registry.get("yt-dlp").is_none(),
        "finish must clear the component state"
    );
    let ffmpeg = registry
        .get("ffmpeg")
        .expect("another component must track independently");
    assert_eq!(ffmpeg.phase, "verifying");
    assert_eq!(ffmpeg.percent, 10);

    registry.finish("ffmpeg");
    assert!(registry.get("ffmpeg").is_none());
}

#[tokio::test]
async fn install_without_trust_or_catalog_target_is_refused_before_work() {
    let signing_key = random_signing_key();
    let public_key = public_key_hex(&signing_key);
    let yt_dlp_bytes: &[u8] = b"yt-dlp-policy-binary";

    let server = KernelServer::start(
        KernelServerConfig::new(b"{}".to_vec()).artifact("yt-dlp", yt_dlp_bytes.to_vec()),
    );
    let components = [
        ComponentSpec {
            name: "yt-dlp",
            version: "2026.11.13",
            sequence: 1,
            license: "Unlicense",
            artifact: ArtifactSpec::binary("yt-dlp", server.artifact_url("yt-dlp"), yt_dlp_bytes),
        },
        unused_ffmpeg_spec(),
        unused_deno_spec(),
    ];
    let catalog = verified_catalog(&signed_catalog(&signing_key, &components), &public_key);

    let root = tempdir().expect("tempdir");
    let db_path = root.path().join("components.db");
    let kernel_root = CapDir::open(root.path().join("kernel")).expect("open kernel root");

    let store = ComponentStore::open(&db_path).expect("open component store");
    let installer = Installer::new(store, kernel_root.clone(), Target::current(), None, true);
    let error = installer
        .install(&catalog, "yt-dlp")
        .await
        .expect_err("an absent trust root must refuse the install");
    assert_eq!(error.code(), "COMPONENT_TRUST_UNCONFIGURED");
    assert_eq!(
        server.request_count(),
        0,
        "an absent trust root must not download"
    );
    assert!(
        !kernel_root.root().join("yt-dlp").exists(),
        "a refused install must not create a version directory"
    );
    let reader = ComponentStore::open(&db_path).expect("open reader connection");
    assert!(
        reader
            .active("yt-dlp")
            .expect("read active yt-dlp")
            .is_none(),
        "a refused install must not record an active version"
    );
    assert!(
        reader
            .installed("yt-dlp")
            .expect("list installed yt-dlp")
            .is_empty(),
        "a refused install must not record any version"
    );

    let store = ComponentStore::open(&db_path).expect("reopen component store");
    let installer = Installer::new(
        store,
        kernel_root.clone(),
        Target::current(),
        Some(public_key),
        true,
    );
    let without_target = catalog_without_current_target();
    let error = installer
        .install(&without_target, "yt-dlp")
        .await
        .expect_err("a catalog without the current target must be refused");
    assert_eq!(error.code(), "COMPONENT_UNSUPPORTED");
    assert_eq!(
        server.request_count(),
        0,
        "a missing current target must not download"
    );
    assert!(
        !kernel_root.root().join("yt-dlp").exists(),
        "an unsupported install must not create a version directory"
    );
    let reader = ComponentStore::open(&db_path).expect("open reader connection");
    assert!(
        reader
            .active("yt-dlp")
            .expect("read active yt-dlp")
            .is_none(),
        "an unsupported install must not record an active version"
    );
}

// ---------------------------------------------------------------------------
// Verified tool resolution and component status projection.
// ---------------------------------------------------------------------------

const YT_DLP_BYTES: &[u8] = b"yt-dlp-resolver-fixture";
const FFMPEG_BYTES: &[u8] = b"ffmpeg-resolver-fixture";
const FFPROBE_BYTES: &[u8] = b"ffprobe-resolver-fixture";
const DENO_BYTES: &[u8] = b"deno-resolver-fixture";

fn ffmpeg_archive() -> Vec<u8> {
    build_stored_zip(&[
        ZipEntry::file("ffmpeg", FFMPEG_BYTES.to_vec()),
        ZipEntry::file("ffprobe", FFPROBE_BYTES.to_vec()),
    ])
}

fn other_target_key() -> String {
    let current = Target::current().as_str();
    ["macos-arm64", "macos-x64", "windows-x64"]
        .into_iter()
        .find(|key| *key != current)
        .expect("a non-current target key exists")
        .to_string()
}

/// One temp kernel root plus a loopback artifact server serving the three
/// component artifacts used by the resolver and status tests.
struct ToolEnv {
    _root: tempfile::TempDir,
    db_path: PathBuf,
    kernel_root: CapDir,
    signing_key: SigningKey,
    public_key: String,
    server: KernelServer,
}

impl ToolEnv {
    fn new() -> Self {
        let server = KernelServer::start(
            KernelServerConfig::new(b"{}".to_vec())
                .artifact("yt-dlp", YT_DLP_BYTES.to_vec())
                .artifact("ffmpeg.zip", ffmpeg_archive())
                .artifact("deno", DENO_BYTES.to_vec()),
        );
        let root = tempdir().expect("tempdir");
        let db_path = root.path().join("components.db");
        let kernel_root = CapDir::open(root.path().join("kernel")).expect("open kernel root");
        let signing_key = random_signing_key();
        let public_key = public_key_hex(&signing_key);
        Self {
            _root: root,
            db_path,
            kernel_root,
            signing_key,
            public_key,
            server,
        }
    }

    fn store(&self) -> ComponentStore {
        ComponentStore::open(&self.db_path).expect("open component store")
    }

    fn yt_dlp_spec(&self, version: &'static str, sequence: u64) -> ComponentSpec {
        ComponentSpec {
            name: "yt-dlp",
            version,
            sequence,
            license: "Unlicense",
            artifact: ArtifactSpec::binary(
                "yt-dlp",
                self.server.artifact_url("yt-dlp"),
                YT_DLP_BYTES,
            ),
        }
    }

    fn ffmpeg_spec(&self, version: &'static str, sequence: u64) -> ComponentSpec {
        let archive = ffmpeg_archive();
        ComponentSpec {
            name: "ffmpeg",
            version,
            sequence,
            license: "LGPL-2.1-or-later",
            artifact: ArtifactSpec::zip(
                ffmpeg_roles(),
                self.server.artifact_url("ffmpeg.zip"),
                &archive,
            ),
        }
    }

    fn deno_spec(&self, version: &'static str, sequence: u64) -> ComponentSpec {
        ComponentSpec {
            name: "deno",
            version,
            sequence,
            license: "MIT",
            artifact: ArtifactSpec::binary("deno", self.server.artifact_url("deno"), DENO_BYTES),
        }
    }

    fn catalog(&self, components: &[ComponentSpec]) -> ComponentCatalog {
        verified_catalog(
            &signed_catalog(&self.signing_key, components),
            &self.public_key,
        )
    }

    /// The v1 catalog: yt-dlp sequence 1, ffmpeg sequence 2, deno sequence 3.
    fn catalog_v1(&self) -> ComponentCatalog {
        self.catalog(&[
            self.yt_dlp_spec("2026.11.13", 1),
            self.ffmpeg_spec("7.1", 2),
            self.deno_spec("2.5.0", 3),
        ])
    }

    /// The v2 catalog: only yt-dlp moves to sequence 2.
    fn catalog_v2(&self) -> ComponentCatalog {
        self.catalog(&[
            self.yt_dlp_spec("2026.11.14", 2),
            self.ffmpeg_spec("7.1", 2),
            self.deno_spec("2.5.0", 3),
        ])
    }
}

async fn install_components(env: &ToolEnv, catalog: &ComponentCatalog, components: &[&str]) {
    let installer = Installer::new(
        env.store(),
        env.kernel_root.clone(),
        Target::current(),
        Some(env.public_key.clone()),
        true,
    );
    for component in components {
        installer
            .install(catalog, component)
            .await
            .unwrap_or_else(|error| panic!("install {component}: {error}"));
    }
}

fn find_status<'a>(statuses: &'a [ComponentStatus], component: &str) -> &'a ComponentStatus {
    statuses
        .iter()
        .find(|status| status.component == component)
        .unwrap_or_else(|| panic!("status for {component}"))
}

fn component_names(statuses: &[ComponentStatus]) -> Vec<&str> {
    statuses
        .iter()
        .map(|status| status.component.as_str())
        .collect()
}

fn catalog_with_current_target_removed(
    mut catalog: ComponentCatalog,
    component: &str,
) -> ComponentCatalog {
    let entry = catalog
        .components
        .get_mut(component)
        .expect("catalog declares the component");
    entry.artifacts.remove(&Target::current());
    catalog
}

#[tokio::test]
async fn resolver_returns_verified_tool_paths() {
    let env = ToolEnv::new();
    let catalog = env.catalog_v1();
    install_components(&env, &catalog, &["yt-dlp", "ffmpeg", "deno"]).await;

    let store = env.store();
    let resolver = ToolResolver::new(&store, &env.kernel_root);
    let resolved: ResolvedTools = resolver.resolve().expect("verified tools must resolve");

    for path in [&resolved.yt_dlp, &resolved.ffmpeg, &resolved.ffprobe] {
        assert!(
            path.is_file(),
            "resolved tool must exist: {}",
            path.display()
        );
        assert!(
            path.starts_with(env.kernel_root.root()),
            "resolved tool must stay under the kernel root: {}",
            path.display()
        );
    }
    assert!(
        resolved.ffmpeg_dir.join("ffmpeg").is_file(),
        "the ffmpeg directory must contain ffmpeg"
    );
    assert!(
        resolved.ffmpeg_dir.join("ffprobe").is_file(),
        "the ffmpeg directory must contain ffprobe"
    );
    let deno = resolved.deno.as_ref().expect("deno must resolve");
    assert!(deno.is_file(), "resolved deno must exist");
    assert!(
        deno.starts_with(env.kernel_root.root()),
        "resolved deno must stay under the kernel root"
    );

    resolver
        .ensure(&[
            ToolRequirement::YtDlp,
            ToolRequirement::Ffmpeg,
            ToolRequirement::Ffprobe,
        ])
        .expect("required tools must satisfy ensure");

    std::fs::remove_file(deno).expect("remove the deno file");
    let error = resolver
        .ensure(&[ToolRequirement::Deno])
        .expect_err("a missing deno file must be refused");
    assert_eq!(error.code(), "COMPONENT_MISSING");
    std::fs::write(deno, DENO_BYTES).expect("restore the deno file");
    resolver
        .ensure(&[ToolRequirement::Deno])
        .expect("a restored deno file must resolve");

    std::fs::remove_file(&resolved.ffprobe).expect("remove the ffprobe file");
    let error = resolver
        .ensure(&[ToolRequirement::Ffprobe])
        .expect_err("a missing ffprobe file must be refused");
    assert_eq!(error.code(), "COMPONENT_MISSING");
    assert!(
        error.message().contains("ffmpeg"),
        "a missing ffprobe must name the ffmpeg component: {}",
        error.message()
    );
    std::fs::write(&resolved.ffprobe, FFPROBE_BYTES).expect("restore the ffprobe file");
    resolver
        .ensure(&[ToolRequirement::Ffprobe])
        .expect("a restored ffprobe file must resolve");
    resolver
        .resolve()
        .expect("restored files must resolve again");
}

#[tokio::test]
async fn resolver_rejects_tamper_target_and_version_mismatch() {
    let env = ToolEnv::new();
    let catalog = env.catalog_v1();
    install_components(&env, &catalog, &["yt-dlp", "ffmpeg", "deno"]).await;

    let store = env.store();
    let original = store
        .active("yt-dlp")
        .expect("read active yt-dlp")
        .expect("yt-dlp must be active");
    let active_file = env
        .kernel_root
        .root()
        .join(&original.version_dir)
        .join("yt-dlp");
    let resolver = ToolResolver::new(&store, &env.kernel_root);
    resolver.resolve().expect("the baseline must resolve");

    std::fs::write(&active_file, b"tampered-bytes").expect("modify a recorded file");
    let error = resolver
        .resolve()
        .expect_err("modified bytes must be refused");
    assert_eq!(error.code(), "COMPONENT_HASH");
    std::fs::write(&active_file, YT_DLP_BYTES).expect("restore the original bytes");
    resolver
        .resolve()
        .expect("restored bytes must resolve again");

    std::fs::remove_file(&active_file).expect("delete a recorded file");
    let error = resolver
        .resolve()
        .expect_err("a missing recorded file must be refused");
    assert_eq!(error.code(), "COMPONENT_MISSING");
    std::fs::write(&active_file, YT_DLP_BYTES).expect("restore the recorded file");

    let mut wrong_target = original.clone();
    wrong_target.target = other_target_key();
    store
        .record_active(&wrong_target)
        .expect("record an install for another target");
    let error = resolver
        .resolve()
        .expect_err("a record from another target must be refused");
    assert_eq!(error.code(), "COMPONENT_UNSUPPORTED");
    store
        .record_active(&original)
        .expect("restore the original target record");

    let mut wrong_directory = original.clone();
    wrong_directory.version_dir = format!("yt-dlp/9-{}", original.version);
    store
        .record_active(&wrong_directory)
        .expect("record an inconsistent version directory");
    let error = resolver
        .resolve()
        .expect_err("an inconsistent version directory must be refused");
    assert_eq!(error.code(), "COMPONENT_VERSION_MISMATCH");
    store
        .record_active(&original)
        .expect("restore the original version record");

    let resolved = resolver
        .resolve()
        .expect("the restored record must resolve again");
    assert!(resolved.yt_dlp.is_file());
}

#[tokio::test]
async fn status_reports_component_states_and_rollback() {
    let env = ToolEnv::new();
    let operations = OperationRegistry::new();
    let store = env.store();

    let manifest_unavailable = component_statuses(
        &store,
        &env.kernel_root,
        None,
        Target::current(),
        &operations,
    )
    .expect("status without a catalog must project");
    assert_eq!(
        component_names(&manifest_unavailable),
        vec!["yt-dlp", "ffmpeg", "deno"],
        "status must return the three components in a stable order"
    );
    for status in &manifest_unavailable {
        assert_eq!(status.state, ComponentState::ManifestUnavailable);
        assert!(status.active_version.is_none());
        assert_eq!(status.target, Target::current().as_str());
    }

    let catalog_v1 = env.catalog_v1();
    let not_installed = component_statuses(
        &store,
        &env.kernel_root,
        Some(&catalog_v1),
        Target::current(),
        &operations,
    )
    .expect("status with a catalog must project");
    assert_eq!(
        component_names(&not_installed),
        vec!["yt-dlp", "ffmpeg", "deno"]
    );
    for status in &not_installed {
        assert_eq!(status.state, ComponentState::NotInstalled);
        assert!(status.active_version.is_none());
        assert!(status.available_version.is_some());
        assert!(status.installed_versions.is_empty());
        assert!(status.rollback_version.is_none());
        assert!(status.error.is_none());
    }

    install_components(&env, &catalog_v1, &["yt-dlp", "ffmpeg", "deno"]).await;
    let store = env.store();
    let installed = component_statuses(
        &store,
        &env.kernel_root,
        Some(&catalog_v1),
        Target::current(),
        &operations,
    )
    .expect("installed status must project");
    for status in &installed {
        assert_eq!(status.state, ComponentState::Installed);
        assert!(status.active_version.is_some());
        assert_eq!(status.installed_versions.len(), 1);
        assert!(status.error.is_none());
    }

    let catalog_v2 = env.catalog_v2();
    let update_available = component_statuses(
        &store,
        &env.kernel_root,
        Some(&catalog_v2),
        Target::current(),
        &operations,
    )
    .expect("update status must project");
    let yt_dlp = find_status(&update_available, "yt-dlp");
    assert_eq!(yt_dlp.state, ComponentState::UpdateAvailable);
    assert_eq!(yt_dlp.active_version.as_deref(), Some("2026.11.13"));
    assert_eq!(yt_dlp.available_version.as_deref(), Some("2026.11.14"));
    assert_eq!(
        find_status(&update_available, "ffmpeg").state,
        ComponentState::Installed
    );

    let installer = Installer::new(
        env.store(),
        env.kernel_root.clone(),
        Target::current(),
        Some(env.public_key.clone()),
        true,
    );
    installer
        .update(&catalog_v2, "yt-dlp")
        .await
        .expect("update yt-dlp to the newer sequence");
    let store = env.store();
    let rollback_available = component_statuses(
        &store,
        &env.kernel_root,
        Some(&catalog_v2),
        Target::current(),
        &operations,
    )
    .expect("rollback status must project");
    let yt_dlp = find_status(&rollback_available, "yt-dlp");
    assert_eq!(yt_dlp.state, ComponentState::RollbackAvailable);
    assert_eq!(yt_dlp.active_version.as_deref(), Some("2026.11.14"));
    assert_eq!(yt_dlp.rollback_version.as_deref(), Some("2026.11.13"));
    let mut installed_versions = yt_dlp.installed_versions.clone();
    installed_versions.sort();
    assert_eq!(
        installed_versions,
        vec!["2026.11.13".to_string(), "2026.11.14".to_string()]
    );

    let active = store
        .active("yt-dlp")
        .expect("read active yt-dlp")
        .expect("yt-dlp must be active");
    let active_file = env
        .kernel_root
        .root()
        .join(&active.version_dir)
        .join("yt-dlp");
    std::fs::remove_file(&active_file).expect("delete the active yt-dlp file");
    let tampered = component_statuses(
        &store,
        &env.kernel_root,
        Some(&catalog_v2),
        Target::current(),
        &operations,
    )
    .expect("tampered status must project");
    let yt_dlp = find_status(&tampered, "yt-dlp");
    assert_eq!(yt_dlp.state, ComponentState::Tampered);
    assert_eq!(
        yt_dlp
            .error
            .as_ref()
            .expect("tamper status must carry an error")
            .code(),
        "COMPONENT_MISSING"
    );
    std::fs::write(&active_file, YT_DLP_BYTES).expect("restore the active yt-dlp file");

    let without_catalog = component_statuses(
        &store,
        &env.kernel_root,
        None,
        Target::current(),
        &operations,
    )
    .expect("status without a catalog must project");
    let yt_dlp = find_status(&without_catalog, "yt-dlp");
    assert_eq!(
        yt_dlp.state,
        ComponentState::Installed,
        "a verified active install stays usable without a catalog"
    );
    assert!(yt_dlp.error.is_none());

    let missing_target = catalog_with_current_target_removed(catalog_v2.clone(), "yt-dlp");
    let unsupported = component_statuses(
        &store,
        &env.kernel_root,
        Some(&missing_target),
        Target::current(),
        &operations,
    )
    .expect("unsupported status must project");
    let yt_dlp = find_status(&unsupported, "yt-dlp");
    assert_eq!(yt_dlp.state, ComponentState::Unsupported);
    assert_eq!(
        yt_dlp
            .error
            .as_ref()
            .expect("unsupported status must carry an error")
            .code(),
        "COMPONENT_UNSUPPORTED"
    );

    operations.begin("yt-dlp");
    operations.update("yt-dlp", "downloading", 42);
    let during_operation = component_statuses(
        &store,
        &env.kernel_root,
        None,
        Target::current(),
        &operations,
    )
    .expect("status during an operation must project");
    let yt_dlp = find_status(&during_operation, "yt-dlp");
    assert_eq!(
        yt_dlp.state,
        ComponentState::Installed,
        "an active operation must not change the state"
    );
    let operation = yt_dlp
        .operation
        .as_ref()
        .expect("an active operation must be exposed");
    assert_eq!(operation.phase, "downloading");
    assert_eq!(operation.percent, 42);
    operations.finish("yt-dlp");
}

#[tokio::test]
async fn installed_component_resolves_without_trust_or_catalog() {
    let env = ToolEnv::new();
    let catalog = env.catalog_v1();
    install_components(&env, &catalog, &["yt-dlp", "ffmpeg", "deno"]).await;

    let store = env.store();
    let before = store
        .active("yt-dlp")
        .expect("read active yt-dlp")
        .expect("yt-dlp must be active");
    let resolved = ToolResolver::new(&store, &env.kernel_root)
        .resolve()
        .expect("verified installs must resolve without a trust key or catalog");
    assert!(resolved.yt_dlp.is_file());
    assert!(resolved.ffmpeg.is_file());
    assert!(resolved.ffprobe.is_file());
    assert!(resolved.deno.expect("deno must resolve").is_file());

    let requests_before = env.server.request_count();
    let installer = Installer::new(
        env.store(),
        env.kernel_root.clone(),
        Target::current(),
        None,
        true,
    );
    let error = installer
        .install(&catalog, "yt-dlp")
        .await
        .expect_err("a fresh install without a trust key must be refused");
    assert_eq!(error.code(), "COMPONENT_TRUST_UNCONFIGURED");
    assert_eq!(
        env.server.request_count(),
        requests_before,
        "a refused install must not download"
    );
    let after = env
        .store()
        .active("yt-dlp")
        .expect("read active yt-dlp")
        .expect("yt-dlp must stay active");
    assert_eq!(
        after, before,
        "a refused install must not change the active record"
    );
}

// ---------------------------------------------------------------------------
// Catalog fetch and artifact download through the authenticated local proxy.
// ---------------------------------------------------------------------------

const PROXY_USER: &str = "oneget-kernel-proxy-user";
const PROXY_PASSWORD: &str = "oneget-kernel-proxy-password-secret";
const WRONG_PASSWORD: &str = "oneget-kernel-proxy-wrong-secret";

fn proxy_test_components() -> [ComponentSpec; 3] {
    [
        ComponentSpec {
            name: "yt-dlp",
            version: "2026.11.13",
            sequence: 4,
            license: "Unlicense",
            artifact: ArtifactSpec::binary(
                "yt-dlp",
                "https://cdn.example.com/yt-dlp",
                b"yt-dlp-proxy-fixture",
            ),
        },
        unused_ffmpeg_spec(),
        unused_deno_spec(),
    ]
}

fn assert_proxy_requests_hide_credentials(
    proxy: &ProxyServer,
    requests: &[proxy_server::ProxyRequest],
) {
    let expected = proxy
        .expected_authorization()
        .expect("the fixture proxy requires credentials")
        .to_string();
    assert!(
        !requests.is_empty(),
        "the proxy must observe every forwarded request"
    );
    for request in requests {
        assert_eq!(
            request.headers.get("proxy-authorization"),
            Some(&expected),
            "the proxy must observe Proxy-Authorization for {}",
            request.target
        );
        let forwarded = request
            .forwarded_headers
            .as_ref()
            .expect("an authenticated request must reach the origin");
        assert!(
            !forwarded.contains_key("proxy-authorization"),
            "proxy credentials must not be forwarded to the origin"
        );
        for (name, value) in forwarded {
            assert!(
                !value.contains(PROXY_USER) && !value.contains(PROXY_PASSWORD),
                "a forwarded header value leaked a proxy credential: {name}"
            );
        }
    }
}

#[tokio::test]
async fn catalog_fetch_through_authenticated_proxy_verifies_and_never_leaks() {
    let signing_key = random_signing_key();
    let public_key = public_key_hex(&signing_key);
    let catalog_bytes = signed_catalog(&signing_key, &proxy_test_components());
    let server = KernelServer::start(KernelServerConfig::new(catalog_bytes.clone()));
    let proxy = ProxyServer::start(Some(ProxyCredentials {
        username: PROXY_USER.to_string(),
        password: PROXY_PASSWORD.to_string(),
    }));
    let config = ProxyConfig {
        url: proxy.url(),
        username: Some(PROXY_USER.to_string()),
        password: Some(PROXY_PASSWORD.to_string()),
    };
    let timeout = Duration::from_secs(5);

    let body = fetch_catalog_with_proxy(&server.catalog_url(), true, timeout, Some(&config))
        .await
        .expect("the catalog must fetch through the authenticated proxy");
    assert_eq!(
        body, catalog_bytes,
        "the fetched catalog must carry the served signed bytes"
    );
    let verified = verify_catalog(&body, Some(&public_key), Target::current(), true)
        .expect("the fetched catalog bytes must verify");
    assert_eq!(verified.components.len(), 3);

    let requests = proxy.requests();
    assert_eq!(
        server.request_count(),
        requests.len(),
        "every proxy-observed request must reach the origin exactly once"
    );
    assert!(requests
        .iter()
        .all(|request| request.target.contains("/catalog.json")));
    assert_proxy_requests_hide_credentials(&proxy, &requests);
}

#[tokio::test]
async fn artifact_download_through_authenticated_proxy_verifies_and_never_leaks() {
    let bytes: &[u8] = b"canonical-component-artifact-proxy";
    let digest = sha256_hex(bytes);
    let size = bytes.len() as u64;
    let server = KernelServer::start(
        KernelServerConfig::new(b"{}".to_vec()).artifact("proxy-artifact.bin", bytes.to_vec()),
    );
    let proxy = ProxyServer::start(Some(ProxyCredentials {
        username: PROXY_USER.to_string(),
        password: PROXY_PASSWORD.to_string(),
    }));
    let config = ProxyConfig {
        url: proxy.url(),
        username: Some(PROXY_USER.to_string()),
        password: Some(PROXY_PASSWORD.to_string()),
    };
    let timeout = Duration::from_secs(5);

    let downloaded = download_artifact_with_proxy(
        &server.artifact_url("proxy-artifact.bin"),
        &digest,
        size,
        true,
        timeout,
        Some(&config),
    )
    .await
    .expect("a matching artifact must download through the authenticated proxy");
    assert_eq!(
        downloaded, bytes,
        "the proxy must deliver the verified artifact bytes"
    );

    let requests = proxy.requests();
    assert_eq!(
        server.request_count(),
        requests.len(),
        "every proxy-observed request must reach the origin exactly once"
    );
    assert!(requests
        .iter()
        .all(|request| request.target.contains("/artifacts/proxy-artifact.bin")));
    assert_proxy_requests_hide_credentials(&proxy, &requests);
}

#[tokio::test]
async fn wrong_proxy_credentials_fail_typed_and_never_reach_the_origin() {
    const ARTIFACT_BYTES: &[u8] = b"canonical-component-artifact-rejected";
    let digest = sha256_hex(ARTIFACT_BYTES);
    let size = ARTIFACT_BYTES.len() as u64;
    let server = KernelServer::start(
        KernelServerConfig::new(b"{}".to_vec())
            .artifact("proxy-rejected.bin", ARTIFACT_BYTES.to_vec()),
    );
    let proxy = ProxyServer::start(Some(ProxyCredentials {
        username: PROXY_USER.to_string(),
        password: PROXY_PASSWORD.to_string(),
    }));
    let wrong = ProxyConfig {
        url: proxy.url(),
        username: Some(PROXY_USER.to_string()),
        password: Some(WRONG_PASSWORD.to_string()),
    };
    let timeout = Duration::from_secs(5);

    let catalog_error =
        fetch_catalog_with_proxy(&server.catalog_url(), true, timeout, Some(&wrong))
            .await
            .expect_err("a wrong proxy password must fail the catalog fetch");
    assert_eq!(catalog_error.code(), "COMPONENT_MANIFEST_UNAVAILABLE");

    let artifact_error = download_artifact_with_proxy(
        &server.artifact_url("proxy-rejected.bin"),
        &digest,
        size,
        true,
        timeout,
        Some(&wrong),
    )
    .await
    .expect_err("a wrong proxy password must fail the artifact download");
    assert_eq!(artifact_error.code(), "COMPONENT_DOWNLOAD_UNAVAILABLE");

    let requests = proxy.requests();
    assert!(
        !requests.is_empty(),
        "the proxy must observe the rejected attempts"
    );
    for request in &requests {
        assert!(
            request.forwarded_headers.is_none(),
            "a rejected request must not be forwarded to the origin: {}",
            request.target
        );
    }
    assert_eq!(
        server.request_count(),
        0,
        "the origin must never receive a credential-bearing request it cannot authenticate"
    );
}
