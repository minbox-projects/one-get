use oneget_lib::platform::secrets::{MemorySecretStore, SecretError, SecretStore, SecretValue};

#[test]
fn memory_store_round_trip_set_get_delete() {
    let store = MemorySecretStore::new();
    let secret = SecretValue::new("s3cr3t-value".to_string()).expect("non-empty secret");

    store.set("token", &secret).expect("set should succeed");

    let loaded = store.get("token").expect("get should succeed");
    let loaded = loaded.expect("value should be present");
    assert_eq!(loaded.expose_secret(), "s3cr3t-value");

    store.delete("token").expect("delete should succeed");
    let after = store.get("token").expect("get after delete should succeed");
    assert!(after.is_none(), "deleted key should return None");
}

#[test]
fn missing_key_returns_ok_none() {
    let store = MemorySecretStore::new();
    let value = store.get("does-not-exist").expect("get should succeed");
    assert!(value.is_none(), "missing key should return Ok(None)");
}

#[test]
fn unavailable_store_returns_secret_unavailable_for_all_operations() {
    let store = MemorySecretStore::unavailable();
    let secret = SecretValue::new("value".to_string()).expect("non-empty secret");

    let get_err = store.get("k").expect_err("get should fail");
    assert_eq!(get_err.code, "secret_unavailable");

    let set_err = store.set("k", &secret).expect_err("set should fail");
    assert_eq!(set_err.code, "secret_unavailable");

    let delete_err = store.delete("k").expect_err("delete should fail");
    assert_eq!(delete_err.code, "secret_unavailable");
}

#[test]
fn secret_value_rejects_empty_with_code_empty_secret() {
    let err = SecretValue::new(String::new()).expect_err("empty secret must be rejected");
    assert_eq!(err.code, "empty_secret");
    // Also ensure the error type is the expected typed error.
    let _: SecretError = err;
}

#[test]
fn delete_missing_is_idempotent() {
    let store = MemorySecretStore::new();
    store
        .delete("missing")
        .expect("first delete of missing key should succeed");
    store
        .delete("missing")
        .expect("second delete of missing key should be idempotent");
}

#[test]
fn secret_value_debug_and_display_never_leak_but_expose_returns_raw() {
    let raw = "super-secret-raw-value";
    let secret = SecretValue::new(raw.to_string()).expect("non-empty secret");

    assert_eq!(secret.expose_secret(), raw);

    let debug = format!("{:?}", secret);
    let display = format!("{}", secret);
    assert!(
        !debug.contains(raw),
        "Debug must not contain the raw secret, got: {debug}"
    );
    assert!(
        !display.contains(raw),
        "Display must not contain the raw secret, got: {display}"
    );
}
