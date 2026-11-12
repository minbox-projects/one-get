use std::fs;
use std::io::Write;

use oneget_lib::platform::paths::AppPaths;
use tempfile::tempdir;

#[test]
fn with_root_creates_required_directories_and_they_are_writable() {
    let temp = tempdir().expect("tempdir");
    let paths = AppPaths::with_root(temp.path()).expect("with_root should succeed");

    let dirs = [
        ("data", &paths.data),
        ("cache", &paths.cache),
        ("logs", &paths.logs),
        ("downloads", &paths.downloads),
        ("backups", &paths.backups),
    ];

    for (name, dir) in dirs {
        assert!(dir.is_absolute(), "{name} path must be absolute: {dir:?}");
        assert!(dir.is_dir(), "{name} directory must exist: {dir:?}");
        let probe = dir.join("probe.txt");
        let mut file = fs::File::create(&probe).expect("create probe file");
        file.write_all(b"ok").expect("write probe");
        let content = fs::read(&probe).expect("read probe");
        assert_eq!(content, b"ok", "{name} directory must be writable");
    }
}

#[test]
fn for_user_returns_absolute_per_user_roots() {
    let paths = AppPaths::for_user().expect("for_user should succeed");
    let dirs = [
        ("data", &paths.data),
        ("cache", &paths.cache),
        ("logs", &paths.logs),
        ("downloads", &paths.downloads),
        ("backups", &paths.backups),
    ];
    for (name, dir) in dirs {
        assert!(dir.is_absolute(), "{name} root must be absolute: {dir:?}");
    }
}

#[test]
fn ensure_is_idempotent() {
    let temp = tempdir().expect("tempdir");
    let paths = AppPaths::with_root(temp.path()).expect("with_root should succeed");
    paths.ensure().expect("first ensure should succeed");
    paths.ensure().expect("second ensure should succeed");

    for dir in [
        &paths.data,
        &paths.cache,
        &paths.logs,
        &paths.downloads,
        &paths.backups,
    ] {
        assert!(
            dir.is_dir(),
            "directory must still exist after ensure: {dir:?}"
        );
    }
}
