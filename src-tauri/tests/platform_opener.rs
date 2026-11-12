//! Behavior tests for the platform opener boundary.

use oneget_lib::platform::opener::open_target;
use tempfile::tempdir;

#[test]
fn open_target_reports_a_missing_target_with_typed_not_found() {
    let dir = tempdir().expect("tempdir");
    let missing = dir.path().join("missing-target.bin");

    let error = open_target(&missing).expect_err("opening a missing target must fail");

    assert_eq!(
        error.code, "not_found",
        "a missing target must report the stable not_found code"
    );
}
