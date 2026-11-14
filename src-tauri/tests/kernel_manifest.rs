//! Behavior tests for signed component catalogs, target selection, artifact
//! policy, trust configuration, and the verified catalog cache.

mod support;

use std::time::{Duration, Instant};

use ed25519_dalek::{Signer, SigningKey};
use oneget_lib::kernel::canonicalize;
use oneget_lib::kernel::catalog::{ArchiveType, Target, CATALOG_SCHEMA_VERSION};
use oneget_lib::kernel::error::KernelError;
use oneget_lib::kernel::source::{fetch_catalog, CatalogCache};
use serde_json::{json, Value};
use support::kernel_server::{KernelServer, KernelServerConfig};

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
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

fn sha256_hex() -> String {
    "0123456789abcdef".repeat(4)
}

/// Sign a catalog object exactly like the existing manifest helper signs JSON:
/// hex signature over the canonical form of the object without `signature`.
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

fn current_target_key() -> String {
    Target::current().as_str().to_string()
}

fn artifacts(name: &str, files: Value, archive: &'static str) -> Value {
    json!({
        "macos-arm64": {
            "url": format!("https://cdn.example.com/{name}/macos-arm64"),
            "sha256": sha256_hex(),
            "size": 2048,
            "archive": archive,
            "files": files.clone(),
        },
        "macos-x64": {
            "url": format!("https://cdn.example.com/{name}/macos-x64"),
            "sha256": sha256_hex(),
            "size": 2048,
            "archive": archive,
            "files": files.clone(),
        },
        "windows-x64": {
            "url": format!("https://cdn.example.com/{name}/windows-x64"),
            "sha256": sha256_hex(),
            "size": 2048,
            "archive": archive,
            "files": files,
        },
    })
}

/// A catalog declaring exactly the three required components and every target.
fn valid_catalog() -> Value {
    json!({
        "schemaVersion": 1,
        "generatedAt": "2026-11-13T00:00:00Z",
        "components": [
            {
                "name": "yt-dlp",
                "version": "2026.11.13",
                "sequence": 1,
                "license": "Unlicense",
                "artifacts": artifacts(
                    "yt-dlp",
                    json!([{ "role": "yt-dlp", "name": "yt-dlp" }]),
                    "binary",
                ),
            },
            {
                "name": "ffmpeg",
                "version": "7.1",
                "sequence": 2,
                "license": "LGPL-2.1-or-later",
                "artifacts": artifacts(
                    "ffmpeg",
                    json!([
                        { "role": "ffmpeg", "name": "ffmpeg" },
                        { "role": "ffprobe", "name": "ffprobe" },
                    ]),
                    "zip",
                ),
            },
            {
                "name": "deno",
                "version": "2.5.0",
                "sequence": 3,
                "license": "MIT",
                "artifacts": artifacts(
                    "deno",
                    json!([{ "role": "deno", "name": "deno" }]),
                    "binary",
                ),
            },
        ],
    })
}

fn expect_typed_error<T>(result: Result<T, KernelError>, context: &str) -> KernelError {
    match result {
        Ok(_) => panic!("{context}: expected a typed failure, got success"),
        Err(error) => {
            assert!(
                !error.code().is_empty(),
                "{context}: KernelError.code must be a stable non-empty string"
            );
            assert!(
                !error.message().is_empty(),
                "{context}: KernelError.message must be non-empty"
            );
            assert!(
                !error.to_string().is_empty(),
                "{context}: KernelError Display must be non-empty"
            );
            let _as_error: &dyn std::error::Error = &error;
            error
        }
    }
}

fn set_current_url(catalog: &mut Value, component: usize, url: &str) {
    let key = current_target_key();
    catalog["components"][component]["artifacts"][key.as_str()]["url"] = json!(url);
}

fn set_current_field(catalog: &mut Value, component: usize, field: &str, value: Value) {
    let key = current_target_key();
    catalog["components"][component]["artifacts"][key.as_str()][field] = value;
}

fn set_current_files(catalog: &mut Value, component: usize, files: Value) {
    set_current_field(catalog, component, "files", files);
}

fn rejected_catalog(
    signing_key: &SigningKey,
    public_key: &str,
    catalog: Value,
    label: &str,
) -> KernelError {
    let bytes = sign_catalog(signing_key, &catalog);
    expect_typed_error(
        oneget_lib::kernel::catalog::verify_catalog(
            &bytes,
            Some(public_key),
            Target::current(),
            false,
        ),
        label,
    )
}

#[test]
fn valid_catalog_selects_current_target_and_round_trips_verified_cache() {
    let signing_key = random_signing_key();
    let public_key = public_key_hex(&signing_key);
    let bytes = sign_catalog(&signing_key, &valid_catalog());

    let catalog = oneget_lib::kernel::catalog::verify_catalog(
        &bytes,
        Some(public_key.as_str()),
        Target::current(),
        false,
    )
    .expect("valid signed catalog must verify");

    assert_eq!(CATALOG_SCHEMA_VERSION, 1);
    assert_eq!(catalog.schema_version, CATALOG_SCHEMA_VERSION);
    assert_eq!(catalog.generated_at, "2026-11-13T00:00:00Z");
    assert_eq!(catalog.components.len(), 3);

    assert_eq!(Target::MacosArm64.as_str(), "macos-arm64");
    assert_eq!(Target::MacosX64.as_str(), "macos-x64");
    assert_eq!(Target::WindowsX64.as_str(), "windows-x64");
    assert_eq!(Target::current().as_str(), current_target_key().as_str());

    let yt_dlp = catalog
        .components
        .get("yt-dlp")
        .expect("catalog must declare yt-dlp");
    assert_eq!(yt_dlp.display_version, "2026.11.13");
    assert_eq!(yt_dlp.sequence, 1);
    assert!(!yt_dlp.license.is_empty());
    assert_eq!(yt_dlp.artifacts.len(), 3);
    let mut target_keys: Vec<&str> = yt_dlp
        .artifacts
        .keys()
        .map(|target| target.as_str())
        .collect();
    target_keys.sort_unstable();
    assert_eq!(
        target_keys,
        vec!["macos-arm64", "macos-x64", "windows-x64"],
        "all declared target keys must parse"
    );

    let selected = yt_dlp
        .artifacts
        .get(&Target::current())
        .expect("the current target artifact must be selectable");
    assert_eq!(
        selected.url,
        format!("https://cdn.example.com/yt-dlp/{}", current_target_key()),
        "only the current target artifact may be selected"
    );
    assert_eq!(selected.sha256.len(), 64);
    assert_eq!(selected.size, 2048);
    assert!(
        matches!(&selected.archive, ArchiveType::Binary),
        "yt-dlp declares a binary archive"
    );
    assert_eq!(selected.files.len(), 1);
    assert_eq!(selected.files[0].role, "yt-dlp");
    assert_eq!(selected.files[0].name, "yt-dlp");

    let ffmpeg = catalog
        .components
        .get("ffmpeg")
        .expect("catalog must declare ffmpeg");
    assert_eq!(ffmpeg.sequence, 2);
    let ffmpeg_artifact = ffmpeg
        .artifacts
        .get(&Target::current())
        .expect("ffmpeg must publish the current target");
    assert!(
        matches!(&ffmpeg_artifact.archive, ArchiveType::Zip),
        "ffmpeg declares a zip archive"
    );
    let mut roles: Vec<&str> = ffmpeg_artifact
        .files
        .iter()
        .map(|file| file.role.as_str())
        .collect();
    roles.sort_unstable();
    assert_eq!(roles, vec!["ffmpeg", "ffprobe"]);

    let deno = catalog
        .components
        .get("deno")
        .expect("catalog must declare deno");
    assert_eq!(deno.sequence, 3);
    assert_eq!(deno.artifacts.len(), 3);

    let directory = tempfile::tempdir().expect("test cache root must be creatable");
    let cache = CatalogCache::new(directory.path());
    let stored = cache
        .store_verified(&bytes, Some(public_key.as_str()), Target::current(), false)
        .expect("a verified catalog must be cached");
    assert_eq!(stored.components.len(), 3);
    assert!(
        directory.path().join("catalog.json").is_file(),
        "store_verified must write catalog.json inside the cache root"
    );

    let loaded = cache
        .load(Some(public_key.as_str()), Target::current(), false)
        .expect("the verified cache must load");
    assert_eq!(loaded.schema_version, catalog.schema_version);
    assert_eq!(loaded.components.len(), catalog.components.len());
    let stored_url = stored
        .components
        .get("yt-dlp")
        .and_then(|entry| entry.artifacts.get(&Target::current()))
        .expect("stored yt-dlp current artifact")
        .url
        .clone();
    let loaded_url = loaded
        .components
        .get("yt-dlp")
        .and_then(|entry| entry.artifacts.get(&Target::current()))
        .expect("loaded yt-dlp current artifact")
        .url
        .clone();
    assert_eq!(loaded_url, stored_url);
    assert_eq!(
        loaded_url,
        format!("https://cdn.example.com/yt-dlp/{}", current_target_key())
    );
}

#[test]
fn rejection_matrix_fails_typed_and_writes_no_cache() {
    let signing_key = random_signing_key();
    let public_key = public_key_hex(&signing_key);
    let valid_bytes = sign_catalog(&signing_key, &valid_catalog());

    // A field changed after signing must invalidate the signature.
    let mut tampered: Value =
        serde_json::from_slice(&valid_bytes).expect("signed catalog must parse");
    tampered["components"][0]["version"] = json!("2099.01.01");
    let tampered_bytes = serde_json::to_vec(&tampered).expect("tampered catalog must serialize");
    expect_typed_error(
        oneget_lib::kernel::catalog::verify_catalog(
            &tampered_bytes,
            Some(public_key.as_str()),
            Target::current(),
            false,
        ),
        "modified field after signing",
    );

    // A different public key must not verify the catalog.
    let wrong_key = random_signing_key();
    let wrong_key_error = expect_typed_error(
        oneget_lib::kernel::catalog::verify_catalog(
            &valid_bytes,
            Some(public_key_hex(&wrong_key).as_str()),
            Target::current(),
            false,
        ),
        "wrong public key",
    );
    assert_ne!(
        wrong_key_error.code(),
        "COMPONENT_TRUST_UNCONFIGURED",
        "a wrong key is a signature failure, not an unconfigured trust root"
    );

    // A repeated JSON key must be rejected by the strict parser.
    let valid_json = String::from_utf8(valid_bytes.clone()).expect("catalog is UTF-8");
    let duplicate = format!(
        "{{\"schemaVersion\":1,{}",
        valid_json.trim_start_matches('{')
    );
    expect_typed_error(
        oneget_lib::kernel::catalog::verify_catalog(
            duplicate.as_bytes(),
            Some(public_key.as_str()),
            Target::current(),
            false,
        ),
        "duplicate JSON key",
    );

    // The top-level signature field is mandatory.
    let unsigned = serde_json::to_vec(&valid_catalog()).expect("unsigned catalog must serialize");
    expect_typed_error(
        oneget_lib::kernel::catalog::verify_catalog(
            &unsigned,
            Some(public_key.as_str()),
            Target::current(),
            false,
        ),
        "missing signature",
    );

    // Input above 1 MiB must fail before any policy or signature work.
    let oversized = vec![b' '; 1024 * 1024 + 1];
    expect_typed_error(
        oneget_lib::kernel::catalog::verify_catalog(
            &oversized,
            Some(public_key.as_str()),
            Target::current(),
            false,
        ),
        "input above 1 MiB",
    );

    // Only schema version 1 is supported.
    let mut unsupported = valid_catalog();
    unsupported["schemaVersion"] = json!(2);
    let unsupported_bytes = sign_catalog(&signing_key, &unsupported);
    expect_typed_error(
        oneget_lib::kernel::catalog::verify_catalog(
            &unsupported_bytes,
            Some(public_key.as_str()),
            Target::current(),
            false,
        ),
        "unsupported schemaVersion",
    );

    // A failed verification must not leave a cached catalog behind.
    let directory = tempfile::tempdir().expect("test cache root must be creatable");
    let cache = CatalogCache::new(directory.path());
    expect_typed_error(
        cache.store_verified(
            &tampered_bytes,
            Some(public_key.as_str()),
            Target::current(),
            false,
        ),
        "store_verified must reject tampered bytes",
    );
    assert!(
        !directory.path().join("catalog.json").exists(),
        "a rejected catalog must not be written to the cache"
    );
}

#[test]
fn a_component_without_the_current_target_is_unsupported_without_fallback() {
    let signing_key = random_signing_key();
    let public_key = public_key_hex(&signing_key);

    let only_target = if matches!(Target::current(), Target::WindowsX64) {
        "macos-arm64"
    } else {
        "windows-x64"
    };
    let mut catalog = valid_catalog();
    catalog["components"][0]["artifacts"]
        .as_object_mut()
        .expect("yt-dlp artifacts must be an object")
        .retain(|key, _| key == only_target);
    assert_eq!(
        catalog["components"][0]["artifacts"]
            .as_object()
            .expect("yt-dlp artifacts must be an object")
            .len(),
        1,
        "the fixture must declare exactly one non-current target"
    );

    let bytes = sign_catalog(&signing_key, &catalog);
    let error = expect_typed_error(
        oneget_lib::kernel::catalog::verify_catalog(
            &bytes,
            Some(public_key.as_str()),
            Target::current(),
            false,
        ),
        "a component missing the current target",
    );
    assert_eq!(
        error.code(),
        "COMPONENT_UNSUPPORTED",
        "the current target's absence must be COMPONENT_UNSUPPORTED, not a fallback"
    );
}

#[test]
fn artifact_url_and_file_path_policy_rejections() {
    let signing_key = random_signing_key();
    let public_key = public_key_hex(&signing_key);

    let mut catalog = valid_catalog();
    set_current_url(&mut catalog, 0, "http://downloads.example.com/yt-dlp");
    rejected_catalog(&signing_key, &public_key, catalog, "non-loopback HTTP URL");

    let mut catalog = valid_catalog();
    set_current_url(
        &mut catalog,
        0,
        "https://user:secret@downloads.example.com/yt-dlp",
    );
    rejected_catalog(&signing_key, &public_key, catalog, "URL with userinfo");

    let mut catalog = valid_catalog();
    set_current_url(
        &mut catalog,
        0,
        "https://downloads.example.com/yt-dlp#fragment",
    );
    rejected_catalog(&signing_key, &public_key, catalog, "URL with fragment");

    let mut catalog = valid_catalog();
    let overlong = format!("https://downloads.example.com/{}", "a".repeat(8200));
    assert!(overlong.len() > 8192, "fixture must exceed the URL limit");
    set_current_url(&mut catalog, 0, &overlong);
    rejected_catalog(&signing_key, &public_key, catalog, "overlong URL");

    let mut catalog = valid_catalog();
    set_current_files(
        &mut catalog,
        0,
        json!([{ "role": "yt-dlp", "name": "/usr/local/bin/yt-dlp" }]),
    );
    rejected_catalog(&signing_key, &public_key, catalog, "absolute file name");

    let mut catalog = valid_catalog();
    set_current_files(
        &mut catalog,
        0,
        json!([{ "role": "yt-dlp", "name": "../yt-dlp" }]),
    );
    rejected_catalog(
        &signing_key,
        &public_key,
        catalog,
        "parent component in file name",
    );

    let mut catalog = valid_catalog();
    set_current_files(
        &mut catalog,
        1,
        json!([
            { "role": "ffmpeg", "name": "tool" },
            { "role": "ffprobe", "name": "TOOL" },
        ]),
    );
    rejected_catalog(
        &signing_key,
        &public_key,
        catalog,
        "case-insensitive duplicate output name",
    );

    let mut catalog = valid_catalog();
    set_current_field(&mut catalog, 0, "archive", json!("tar"));
    rejected_catalog(
        &signing_key,
        &public_key,
        catalog,
        "unsupported archive type",
    );

    let mut catalog = valid_catalog();
    set_current_files(
        &mut catalog,
        1,
        json!([{ "role": "ffmpeg", "name": "ffmpeg" }]),
    );
    rejected_catalog(
        &signing_key,
        &public_key,
        catalog,
        "missing ffmpeg component role",
    );

    let mut catalog = valid_catalog();
    set_current_files(
        &mut catalog,
        1,
        json!([
            { "role": "ffmpeg", "name": "ffmpeg" },
            { "role": "ffprobe", "name": "ffprobe" },
            { "role": "ffplay", "name": "ffplay" },
        ]),
    );
    rejected_catalog(
        &signing_key,
        &public_key,
        catalog,
        "extra ffmpeg component role",
    );

    // Loopback HTTP is test-only and must follow allow_loopback_http.
    let mut catalog = valid_catalog();
    set_current_url(&mut catalog, 0, "http://127.0.0.1:8080/yt-dlp");
    let bytes = sign_catalog(&signing_key, &catalog);
    expect_typed_error(
        oneget_lib::kernel::catalog::verify_catalog(
            &bytes,
            Some(public_key.as_str()),
            Target::current(),
            false,
        ),
        "loopback HTTP without the test opt-in",
    );
    let accepted = oneget_lib::kernel::catalog::verify_catalog(
        &bytes,
        Some(public_key.as_str()),
        Target::current(),
        true,
    )
    .expect("loopback HTTP must be accepted with allow_loopback_http");
    let accepted_url = accepted
        .components
        .get("yt-dlp")
        .and_then(|entry| entry.artifacts.get(&Target::current()))
        .expect("accepted yt-dlp current artifact")
        .url
        .clone();
    assert_eq!(accepted_url, "http://127.0.0.1:8080/yt-dlp");
}

#[test]
fn unconfigured_trust_is_distinct_from_a_wrong_key() {
    let signing_key = random_signing_key();
    let bytes = sign_catalog(&signing_key, &valid_catalog());

    let absent = expect_typed_error(
        oneget_lib::kernel::catalog::verify_catalog(&bytes, None, Target::current(), false),
        "no configured trust key",
    );
    assert_eq!(absent.code(), "COMPONENT_TRUST_UNCONFIGURED");
    assert!(!absent.message().is_empty());

    let empty = expect_typed_error(
        oneget_lib::kernel::catalog::verify_catalog(&bytes, Some(""), Target::current(), false),
        "empty configured trust key",
    );
    assert_eq!(empty.code(), "COMPONENT_TRUST_UNCONFIGURED");

    let wrong_key = random_signing_key();
    let wrong_key_error = expect_typed_error(
        oneget_lib::kernel::catalog::verify_catalog(
            &bytes,
            Some(public_key_hex(&wrong_key).as_str()),
            Target::current(),
            false,
        ),
        "catalog signed by a different key",
    );
    assert!(
        !wrong_key_error.code().is_empty(),
        "a wrong key must still fail with a typed code"
    );
    assert_ne!(
        wrong_key_error.code(),
        "COMPONENT_TRUST_UNCONFIGURED",
        "a configured but wrong key must not be reported as unconfigured trust"
    );
}

#[test]
fn cache_load_reverifies_and_leaves_no_temporary_files() {
    let signing_key = random_signing_key();
    let public_key = public_key_hex(&signing_key);
    let bytes = sign_catalog(&signing_key, &valid_catalog());
    let directory = tempfile::tempdir().expect("test cache root must be creatable");
    let cache = CatalogCache::new(directory.path());

    let missing = expect_typed_error(
        cache.load(Some(public_key.as_str()), Target::current(), false),
        "missing cache",
    );
    assert_eq!(missing.code(), "COMPONENT_MANIFEST_UNAVAILABLE");

    cache
        .store_verified(&bytes, Some(public_key.as_str()), Target::current(), false)
        .expect("a verified catalog must be cached");
    let mut names: Vec<String> = std::fs::read_dir(directory.path())
        .expect("cache root must be readable")
        .map(|entry| {
            entry
                .expect("cache entry must be readable")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    names.sort();
    assert_eq!(
        names,
        vec!["catalog.json".to_string()],
        "a successful store must leave only catalog.json, with no temporary leftovers"
    );

    cache
        .load(Some(public_key.as_str()), Target::current(), false)
        .expect("stored catalog must load");

    let cached_path = directory.path().join("catalog.json");
    let mut tampered: Value =
        serde_json::from_slice(&std::fs::read(&cached_path).expect("cache file must be readable"))
            .expect("cached catalog must parse");
    tampered["components"][0]["version"] = json!("2099.01.01");
    std::fs::write(
        &cached_path,
        serde_json::to_vec(&tampered).expect("tampered cache must serialize"),
    )
    .expect("tampered cache must be writable");

    let tampered_error = expect_typed_error(
        cache.load(Some(public_key.as_str()), Target::current(), false),
        "tampered cache must fail re-verification",
    );
    assert_ne!(
        tampered_error.code(),
        "COMPONENT_MANIFEST_UNAVAILABLE",
        "a present but tampered cache is a verification failure, not a missing manifest"
    );
    assert_ne!(
        tampered_error.code(),
        "COMPONENT_TRUST_UNCONFIGURED",
        "a tampered cache is not an unconfigured trust root"
    );
}

fn manifest_tool() -> std::process::Command {
    std::process::Command::new(env!("CARGO_BIN_EXE_kernel_manifest"))
}

fn is_lowercase_hex_64(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn combined_output(output: &std::process::Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn manifest_tool_keygen_sign_verify_round_trip() {
    // The key material lives in a tempdir outside the repository.
    let directory = tempfile::tempdir().expect("tempdir outside the repository must be creatable");
    let private_key_path = directory.path().join("private.key");
    let unsigned_path = directory.path().join("unsigned.json");
    let signed_path = directory.path().join("signed.json");

    let keygen = manifest_tool()
        .arg("keygen")
        .arg("--private-key")
        .arg(&private_key_path)
        .output()
        .expect("keygen must run");
    assert!(
        keygen.status.success(),
        "keygen must succeed: {}",
        String::from_utf8_lossy(&keygen.stderr)
    );
    assert!(
        String::from_utf8_lossy(&keygen.stdout).ends_with('\n'),
        "keygen must print the public key plus a newline"
    );
    let public_key = String::from_utf8(keygen.stdout.clone())
        .expect("keygen stdout is UTF-8")
        .trim()
        .to_string();
    assert!(
        is_lowercase_hex_64(&public_key),
        "keygen must print 64 lowercase hex characters"
    );

    let private_key = std::fs::read_to_string(&private_key_path)
        .expect("the generated private key file must be readable")
        .trim()
        .to_string();
    assert!(
        is_lowercase_hex_64(&private_key),
        "the stored private seed must be 64 lowercase hex characters"
    );
    assert_ne!(
        private_key, public_key,
        "the private seed must differ from the printed public key"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&private_key_path)
            .expect("private key metadata must be readable")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o077,
            0,
            "the private key must not grant group or other permissions"
        );
    }

    let unsigned = valid_catalog();
    std::fs::write(
        &unsigned_path,
        serde_json::to_vec(&unsigned).expect("the unsigned catalog must serialize"),
    )
    .expect("the unsigned catalog must be writable");

    let sign = manifest_tool()
        .arg("sign")
        .arg("--private-key")
        .arg(&private_key_path)
        .arg("--input")
        .arg(&unsigned_path)
        .arg("--output")
        .arg(&signed_path)
        .output()
        .expect("sign must run");
    assert!(
        sign.status.success(),
        "sign must succeed: {}",
        String::from_utf8_lossy(&sign.stderr)
    );

    let signed_bytes = std::fs::read(&signed_path).expect("the signed output must be readable");
    let signed: Value =
        serde_json::from_slice(&signed_bytes).expect("the signed output must be a JSON object");
    let signature = signed
        .get("signature")
        .and_then(Value::as_str)
        .expect("the signed output must carry a signature field");
    assert!(
        signature.len() == 128
            && signature
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')),
        "the signature must be 128 lowercase hex characters"
    );
    assert_eq!(
        signed["schemaVersion"],
        json!(1),
        "sign must preserve the unsigned catalog content"
    );
    oneget_lib::kernel::catalog::verify_catalog(
        &signed_bytes,
        Some(public_key.as_str()),
        Target::current(),
        false,
    )
    .expect("the signed output must verify with the same canonicalization the kernel uses");

    let verify = manifest_tool()
        .arg("verify")
        .arg("--public-key")
        .arg(&public_key)
        .arg("--input")
        .arg(&signed_path)
        .output()
        .expect("verify must run");
    assert!(
        verify.status.success(),
        "a valid signature must verify: {}",
        String::from_utf8_lossy(&verify.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&verify.stdout).trim(), "ok");

    let mut tampered = signed;
    tampered["components"][0]["version"] = json!("2099.01.01");
    std::fs::write(
        &signed_path,
        serde_json::to_vec(&tampered).expect("the tampered catalog must serialize"),
    )
    .expect("the tampered catalog must be writable");

    let rejected = manifest_tool()
        .arg("verify")
        .arg("--public-key")
        .arg(&public_key)
        .arg("--input")
        .arg(&signed_path)
        .output()
        .expect("verify must run on tampered input");
    assert!(
        !rejected.status.success(),
        "a modified signed field must fail verification"
    );

    for (label, output) in [
        ("keygen", &keygen),
        ("sign", &sign),
        ("verify", &verify),
        ("tampered verify", &rejected),
    ] {
        assert!(
            !combined_output(output).contains(&private_key),
            "{label} must never print the private key"
        );
    }
}

#[tokio::test]
async fn catalog_fetch_returns_raw_signed_bytes_from_loopback() {
    let signing_key = random_signing_key();
    let public_key = public_key_hex(&signing_key);
    let signed = sign_catalog(&signing_key, &valid_catalog());

    let server = KernelServer::start(KernelServerConfig::new(signed.clone()));
    let fetched = fetch_catalog(&server.catalog_url(), true, Duration::from_secs(2))
        .await
        .expect("loopback fetch with the test opt-in must succeed");

    assert_eq!(
        fetched, signed,
        "fetch_catalog must return the raw served bytes for the caller to verify"
    );
    oneget_lib::kernel::catalog::verify_catalog(
        &fetched,
        Some(public_key.as_str()),
        Target::current(),
        true,
    )
    .expect("the fetched bytes must verify as a signed catalog");
    assert_eq!(
        server.request_count(),
        1,
        "one catalog fetch must make exactly one request"
    );
}

#[tokio::test]
async fn catalog_fetch_rejects_policy_timeout_and_oversized_body() {
    // A plain HTTP loopback URL is rejected before any connection when the test
    // opt-in is absent, so the fixture records no request.
    let server = KernelServer::start(KernelServerConfig::new(b"{}".to_vec()));
    let policy_error = expect_typed_error(
        fetch_catalog(&server.catalog_url(), false, Duration::from_millis(200)).await,
        "loopback HTTP without the test opt-in",
    );
    assert!(!policy_error.code().is_empty());
    assert_eq!(
        server.request_count(),
        0,
        "a URL policy rejection must not contact the server"
    );
    drop(server);

    // A non-loopback plain HTTP URL is outside the artifact URL policy even
    // when the loopback test opt-in is enabled.
    let non_loopback_error = expect_typed_error(
        fetch_catalog(
            "http://198.51.100.7/catalog.json",
            true,
            Duration::from_millis(200),
        )
        .await,
        "non-loopback plain HTTP URL",
    );
    assert!(!non_loopback_error.code().is_empty());

    // A response body above 1 MiB is a typed error, not a truncated success.
    let oversized = KernelServer::start(
        KernelServerConfig::new(Vec::new()).with_oversized_catalog(1024 * 1024 + 1),
    );
    let oversized_error = expect_typed_error(
        fetch_catalog(&oversized.catalog_url(), true, Duration::from_secs(2)).await,
        "catalog body above 1 MiB",
    );
    assert!(!oversized_error.code().is_empty());
    drop(oversized);

    // A server delay longer than the injected timeout is a typed error within a
    // generous wall-clock bound; exact durations are not asserted.
    let delayed = KernelServer::start(
        KernelServerConfig::new(b"{}".to_vec()).with_delay(Duration::from_secs(3)),
    );
    let started = Instant::now();
    let timeout_error = expect_typed_error(
        fetch_catalog(&delayed.catalog_url(), true, Duration::from_millis(150)).await,
        "server delay longer than the injected timeout",
    );
    let elapsed = started.elapsed();
    assert!(!timeout_error.code().is_empty());
    assert!(
        delayed.request_count() >= 1,
        "the timeout case must have reached the fixture server"
    );
    assert!(
        elapsed < Duration::from_secs(5),
        "the injected timeout must return within a generous bound, took {elapsed:?}"
    );
    drop(delayed);
}
