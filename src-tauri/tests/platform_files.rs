use oneget_lib::platform::files::CapDir;
use tempfile::tempdir;

#[tokio::test]
async fn capdir_supports_create_write_sync_read_metadata_and_replace() {
    let root = tempdir().expect("tempdir");
    let cap = CapDir::open(root.path()).expect("open capdir");

    cap.create("source.txt").await.expect("create source");
    cap.write("source.txt", b"new-content")
        .await
        .expect("write source");
    cap.sync("source.txt").await.expect("sync source");

    cap.create("dest.txt").await.expect("create dest");
    cap.write("dest.txt", b"old-content")
        .await
        .expect("write dest");
    cap.sync("dest.txt").await.expect("sync dest");

    let read_back = cap.read("source.txt").await.expect("read source");
    assert_eq!(read_back, b"new-content");

    let meta = cap.metadata("source.txt").await.expect("metadata source");
    assert!(meta.is_file, "metadata should report a file");
    assert_eq!(meta.len, b"new-content".len() as u64);

    cap.replace("source.txt", "dest.txt")
        .await
        .expect("replace destination atomically");

    let dest_after = cap
        .read("dest.txt")
        .await
        .expect("read destination after replace");
    assert_eq!(
        dest_after, b"new-content",
        "destination must contain new content"
    );

    let source_after = cap.read("source.txt").await;
    assert!(source_after.is_err(), "source must be absent after replace");
}

#[tokio::test]
async fn capdir_rejects_escape_paths_and_does_not_create_outside_files() {
    use std::fs;

    let root = tempdir().expect("tempdir");
    let cap = CapDir::open(root.path()).expect("open capdir");

    let outside_relative = root
        .path()
        .parent()
        .expect("tempdir parent")
        .join("oneget-platform-files-outside-relative-probe");
    let _ = fs::remove_file(&outside_relative);

    let traversal = cap
        .create("../oneget-platform-files-outside-relative-probe")
        .await;
    assert!(traversal.is_err(), "../ path must be rejected");
    assert!(
        !outside_relative.exists(),
        "traversal attempt must not create an outside file"
    );

    let outside_absolute =
        std::env::temp_dir().join("oneget-platform-files-outside-absolute-probe");
    let _ = fs::remove_file(&outside_absolute);
    let abs_path = outside_absolute.to_string_lossy().to_string();
    let absolute = cap.write(&abs_path, b"nope").await;
    assert!(absolute.is_err(), "absolute path must be rejected");
    assert!(
        !outside_absolute.exists(),
        "absolute path attempt must not create an outside file"
    );

    let nul = cap.create("bad\0name").await;
    assert!(nul.is_err(), "NUL path must be rejected");
}

#[cfg(unix)]
#[tokio::test]
async fn capdir_rejects_symlink_escape_and_leaves_outside_file_unchanged() {
    use std::fs;
    use std::os::unix::fs::symlink;

    let root = tempdir().expect("tempdir");
    let outside = tempdir().expect("outside tempdir");
    let outside_file = outside.path().join("secret.txt");
    fs::write(&outside_file, b"outside-original").expect("write outside file");

    let link_path = root.path().join("escape-link");
    symlink(&outside_file, &link_path).expect("create symlink");

    let cap = CapDir::open(root.path()).expect("open capdir");

    let read_result = cap.read("escape-link").await;
    assert!(
        read_result.is_err(),
        "read through symlink escape must be rejected"
    );

    let create_result = cap.create("escape-link").await;
    assert!(
        create_result.is_err(),
        "create through symlink escape must be rejected"
    );

    let outside_content = fs::read(&outside_file).expect("read outside file");
    assert_eq!(
        outside_content, b"outside-original",
        "outside file must remain unchanged"
    );
}

#[cfg(windows)]
#[tokio::test]
async fn capdir_rejects_junction_escape_and_leaves_outside_file_unchanged() {
    use std::fs;
    use std::process::Command;

    let root = tempdir().expect("tempdir");
    let outside = tempdir().expect("outside tempdir");
    let outside_file = outside.path().join("secret.txt");
    fs::write(&outside_file, b"outside-original").expect("write outside file");

    let junction_path = root.path().join("escape-junction");
    let status = Command::new("cmd")
        .args([
            "/C",
            "mklink",
            "/J",
            &junction_path.to_string_lossy(),
            &outside.path().to_string_lossy(),
        ])
        .status()
        .expect("run mklink /J");
    assert!(status.success(), "failed to create directory junction");

    let cap = CapDir::open(root.path()).expect("open capdir");

    let read_result = cap.read("escape-junction\\secret.txt").await;
    assert!(
        read_result.is_err(),
        "read through junction escape must be rejected"
    );

    let create_result = cap.create("escape-junction\\new.txt").await;
    assert!(
        create_result.is_err(),
        "create through junction escape must be rejected"
    );

    let outside_content = fs::read(&outside_file).expect("read outside file");
    assert_eq!(
        outside_content, b"outside-original",
        "outside file must remain unchanged"
    );
    assert!(
        !outside.path().join("new.txt").exists(),
        "outside directory must not gain files through rejected junction operations"
    );
}
