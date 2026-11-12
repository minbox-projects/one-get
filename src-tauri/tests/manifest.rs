use std::panic::{catch_unwind, AssertUnwindSafe};

use ed25519_dalek::{Signer, SigningKey};
use oneget_lib::kernel::manifest::{
    canonicalize, verify_manifest, ManifestError, MAX_MANIFEST_BYTES,
};
use serde_json::{json, Value};

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
    getrandom::getrandom(&mut seed)
        .expect("os randomness must be available for test key generation");
    SigningKey::from_bytes(&seed)
}

fn sign_manifest_object(signing_key: &SigningKey, manifest: &Value) -> (String, String) {
    let canonical = canonicalize(manifest).expect("test manifest must canonicalize");
    let signature = signing_key.sign(canonical.as_bytes());
    let public_key_hex = hex_encode(signing_key.verifying_key().as_bytes());
    let signature_hex = hex_encode(&signature.to_bytes());
    (public_key_hex, signature_hex)
}

fn signed_manifest_value(manifest: &Value, signature_hex: &str) -> Value {
    let mut signed = manifest
        .as_object()
        .expect("test manifest must be an object")
        .clone();
    signed.insert(
        "signature".to_string(),
        Value::String(signature_hex.to_string()),
    );
    Value::Object(signed)
}

fn assert_typed_error(result: Result<Value, ManifestError>, context: &str) -> ManifestError {
    let err = result.expect_err(context);
    assert!(
        !err.code.is_empty(),
        "{context}: ManifestError.code must be a stable non-empty string"
    );
    assert!(
        !err.message.is_empty(),
        "{context}: ManifestError.message must be non-empty"
    );
    let display = err.to_string();
    assert!(
        !display.is_empty(),
        "{context}: ManifestError Display must be non-empty"
    );
    let _as_error: &dyn std::error::Error = &err;
    err
}

fn sample_manifest() -> Value {
    json!({
        "name": "oneget-component",
        "version": 1,
        "nested": { "b": 2, "a": 1 },
        "files": ["a", "b"]
    })
}

#[test]
fn valid_manifest_is_accepted_and_signature_excluded() {
    let signing_key = random_signing_key();
    let manifest = sample_manifest();
    let (public_key_hex, signature_hex) = sign_manifest_object(&signing_key, &manifest);
    let signed = signed_manifest_value(&manifest, &signature_hex);
    let bytes = serde_json::to_vec(&signed).expect("signed manifest must serialize");

    let verified = verify_manifest(&bytes, &public_key_hex).expect("valid manifest must verify");

    assert_eq!(
        verified, manifest,
        "verified value must contain original fields"
    );
    assert!(
        verified.get("signature").is_none(),
        "verified value must exclude top-level signature"
    );
}

#[test]
fn reordered_keys_and_whitespace_still_verify() {
    let signing_key = random_signing_key();
    let manifest = sample_manifest();
    let (public_key_hex, signature_hex) = sign_manifest_object(&signing_key, &manifest);

    // Same signed payload as canonicalized content but submitted as a raw JSON document
    // with reordered keys and arbitrary whitespace.
    let raw = format!(
        "{{\n  \"version\" : 1,\n  \"signature\" : \"{signature_hex}\" ,\n  \"nested\" : {{ \"a\":1,  \"b\" : 2 }},\n  \"name\" : \"oneget-component\",\n  \"files\" : [ \"a\", \"b\" ]\n}}"
    );

    let verified = verify_manifest(raw.as_bytes(), &public_key_hex)
        .expect("reordered keys and whitespace must still verify");

    assert_eq!(verified, manifest);
    assert!(verified.get("signature").is_none());
}

#[test]
fn modified_field_is_rejected_with_typed_error() {
    let signing_key = random_signing_key();
    let manifest = sample_manifest();
    let (public_key_hex, signature_hex) = sign_manifest_object(&signing_key, &manifest);

    let mut tampered = signed_manifest_value(&manifest, &signature_hex);
    tampered["version"] = json!(2);
    let bytes = serde_json::to_vec(&tampered).expect("tampered manifest must serialize");

    let err = assert_typed_error(
        verify_manifest(&bytes, &public_key_hex),
        "modified field must be rejected",
    );
    assert!(
        err.code.contains("signature") && err.code.contains("mismatch")
            || err.code == "signature_mismatch"
            || err.code == "invalid_signature",
        "modified field should produce a signature verification error, got code={:?} message={:?}",
        err.code,
        err.message
    );
}

#[test]
fn wrong_public_key_is_rejected_with_typed_error() {
    let signing_key = random_signing_key();
    let wrong_key = random_signing_key();
    let manifest = sample_manifest();
    let (_public_key_hex, signature_hex) = sign_manifest_object(&signing_key, &manifest);
    let wrong_public_key_hex = hex_encode(wrong_key.verifying_key().as_bytes());
    let signed = signed_manifest_value(&manifest, &signature_hex);
    let bytes = serde_json::to_vec(&signed).expect("signed manifest must serialize");

    let err = assert_typed_error(
        verify_manifest(&bytes, &wrong_public_key_hex),
        "wrong public key must be rejected",
    );
    assert!(
        err.code.contains("signature") || err.code.contains("mismatch") || err.code.contains("key"),
        "wrong public key should produce a typed verification/key error, got code={:?} message={:?}",
        err.code,
        err.message
    );
}

#[test]
fn malformed_signature_hex_is_rejected_with_typed_error() {
    let signing_key = random_signing_key();
    let manifest = sample_manifest();
    let (public_key_hex, _signature_hex) = sign_manifest_object(&signing_key, &manifest);

    let signed = signed_manifest_value(&manifest, "not-hex!!");
    let bytes = serde_json::to_vec(&signed).expect("manifest must serialize");

    let err = assert_typed_error(
        verify_manifest(&bytes, &public_key_hex),
        "malformed signature hex must be rejected",
    );
    assert!(
        err.code.contains("signature") || err.code.contains("invalid") || err.code.contains("malformed"),
        "malformed signature hex should produce a typed signature error, got code={:?} message={:?}",
        err.code,
        err.message
    );
}

#[test]
fn malformed_public_key_is_rejected_with_typed_error() {
    let signing_key = random_signing_key();
    let manifest = sample_manifest();
    let (_public_key_hex, signature_hex) = sign_manifest_object(&signing_key, &manifest);
    let signed = signed_manifest_value(&manifest, &signature_hex);
    let bytes = serde_json::to_vec(&signed).expect("manifest must serialize");

    let err = assert_typed_error(
        verify_manifest(&bytes, "zz-not-a-public-key"),
        "malformed public key must be rejected",
    );
    assert!(
        err.code.contains("public") || err.code.contains("key") || err.code.contains("invalid"),
        "malformed public key should produce a typed public-key error, got code={:?} message={:?}",
        err.code,
        err.message
    );
}

#[test]
fn non_object_roots_are_rejected_with_typed_error() {
    let signing_key = random_signing_key();
    let public_key_hex = hex_encode(signing_key.verifying_key().as_bytes());

    for (label, bytes) in [
        ("array", br#"[1, 2, 3]"#.as_slice()),
        ("string", br#""hello""#.as_slice()),
        ("number", br#"42"#.as_slice()),
    ] {
        let result = catch_unwind(AssertUnwindSafe(|| verify_manifest(bytes, &public_key_hex)));
        let result = result.unwrap_or_else(|_| panic!("non-object root ({label}) must not panic"));
        let err = assert_typed_error(
            result,
            &format!("non-object root ({label}) must be rejected"),
        );
        assert!(
            err.code.contains("object") || err.code.contains("root") || err.code.contains("non_object"),
            "non-object root ({label}) should produce a typed non-object-root error, got code={:?} message={:?}",
            err.code,
            err.message
        );
    }
}

#[test]
fn duplicate_top_level_key_is_rejected_with_typed_error() {
    let signing_key = random_signing_key();
    let public_key_hex = hex_encode(signing_key.verifying_key().as_bytes());
    let raw = br#"{"name":"x","name":"y","signature":"00"}"#;

    let result = catch_unwind(AssertUnwindSafe(|| verify_manifest(raw, &public_key_hex)));
    let result = result.unwrap_or_else(|_| panic!("duplicate top-level key must not panic"));
    let err = assert_typed_error(result, "duplicate top-level key must be rejected");
    assert!(
        err.code.contains("duplicate") || err.code.contains("key"),
        "duplicate top-level key should produce a typed duplicate-key error, got code={:?} message={:?}",
        err.code,
        err.message
    );
}

#[test]
fn duplicate_nested_key_is_rejected_with_typed_error() {
    let signing_key = random_signing_key();
    let public_key_hex = hex_encode(signing_key.verifying_key().as_bytes());
    let raw = br#"{"nested":{"a":1,"a":2},"signature":"00"}"#;

    let result = catch_unwind(AssertUnwindSafe(|| verify_manifest(raw, &public_key_hex)));
    let result = result.unwrap_or_else(|_| panic!("duplicate nested key must not panic"));
    let err = assert_typed_error(result, "duplicate nested key must be rejected");
    assert!(
        err.code.contains("duplicate") || err.code.contains("key"),
        "duplicate nested key should produce a typed duplicate-key error, got code={:?} message={:?}",
        err.code,
        err.message
    );
}

#[test]
fn oversized_input_is_rejected_with_typed_error() {
    let signing_key = random_signing_key();
    let public_key_hex = hex_encode(signing_key.verifying_key().as_bytes());
    let oversized = vec![b' '; MAX_MANIFEST_BYTES + 1];

    let result = catch_unwind(AssertUnwindSafe(|| {
        verify_manifest(&oversized, &public_key_hex)
    }));
    let result = result.unwrap_or_else(|_| panic!("oversized input must not panic"));
    let err = assert_typed_error(result, "oversized input must be rejected");
    assert!(
        err.code.contains("size")
            || err.code.contains("oversized")
            || err.code.contains("large")
            || err.code.contains("limit"),
        "oversized input should produce a typed oversized error, got code={:?} message={:?}",
        err.code,
        err.message
    );
}

#[test]
fn missing_or_invalid_signature_field_is_rejected_with_typed_error() {
    let signing_key = random_signing_key();
    let manifest = sample_manifest();
    let (public_key_hex, _signature_hex) = sign_manifest_object(&signing_key, &manifest);

    // Missing signature
    let no_sig_bytes = serde_json::to_vec(&manifest).expect("manifest must serialize");
    let err = assert_typed_error(
        verify_manifest(&no_sig_bytes, &public_key_hex),
        "missing signature must be rejected",
    );
    assert!(
        err.code.contains("signature")
            || err.code.contains("missing")
            || err.code.contains("invalid"),
        "missing signature should produce a typed signature error, got code={:?} message={:?}",
        err.code,
        err.message
    );

    // Invalid signature field type (not a string)
    let mut bad_sig = manifest
        .as_object()
        .expect("manifest must be object")
        .clone();
    bad_sig.insert("signature".to_string(), json!(12345));
    let bad_sig_bytes =
        serde_json::to_vec(&Value::Object(bad_sig)).expect("manifest must serialize");
    let result = catch_unwind(AssertUnwindSafe(|| {
        verify_manifest(&bad_sig_bytes, &public_key_hex)
    }));
    let result = result.unwrap_or_else(|_| panic!("non-string signature field must not panic"));
    let err = assert_typed_error(result, "non-string signature field must be rejected");
    assert!(
        err.code.contains("signature") || err.code.contains("invalid") || err.code.contains("missing"),
        "invalid signature field type should produce a typed signature error, got code={:?} message={:?}",
        err.code,
        err.message
    );
}

#[test]
fn invalid_json_is_rejected_with_typed_error() {
    let signing_key = random_signing_key();
    let public_key_hex = hex_encode(signing_key.verifying_key().as_bytes());
    let bytes = br#"{ not valid json"#;

    let result = catch_unwind(AssertUnwindSafe(|| verify_manifest(bytes, &public_key_hex)));
    let result = result.unwrap_or_else(|_| panic!("invalid JSON must not panic"));
    let err = assert_typed_error(result, "invalid JSON must be rejected");
    assert!(
        err.code.contains("json") || err.code.contains("parse") || err.code.contains("invalid"),
        "invalid JSON should produce a typed invalid-JSON error, got code={:?} message={:?}",
        err.code,
        err.message
    );
}

#[test]
fn max_manifest_bytes_constant_is_one_mib() {
    assert_eq!(
        MAX_MANIFEST_BYTES,
        1024 * 1024,
        "MAX_MANIFEST_BYTES must be 1 MiB"
    );
}
