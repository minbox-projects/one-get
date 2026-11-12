use std::time::Duration;

use oneget_lib::platform::process::ChildGuard;

#[cfg(unix)]
mod unix {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::process::Command as StdCommand;
    use std::thread;
    use std::time::Instant;
    use tempfile::tempdir;
    use tokio::process::Command;

    fn poll_for_pidfile(path: &Path, timeout: Duration) -> u32 {
        let start = Instant::now();
        loop {
            if let Ok(contents) = fs::read_to_string(path) {
                let trimmed = contents.trim();
                if !trimmed.is_empty() {
                    return trimmed.parse::<u32>().expect("grandchild pid should parse");
                }
            }
            assert!(
                start.elapsed() < timeout,
                "timed out waiting for pidfile {}",
                path.display()
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn process_exists(pid: u32) -> bool {
        StdCommand::new("ps")
            .args(["-p", &pid.to_string()])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn assert_process_gone(pid: u32, timeout: Duration, label: &str) {
        let start = Instant::now();
        while process_exists(pid) {
            assert!(
                start.elapsed() < timeout,
                "{label} process {pid} still running after {:?}",
                timeout
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn spawn_sleep_tree(pidfile: &Path) -> Command {
        let script = format!("sleep 30 & echo $! > '{}'; wait", pidfile.display());
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script);
        cmd
    }

    #[tokio::test]
    async fn kill_tree_terminates_child_and_grandchild() {
        let dir = tempdir().expect("tempdir");
        let pidfile = dir.path().join("grandchild.pid");
        let mut guard =
            ChildGuard::spawn(spawn_sleep_tree(&pidfile)).expect("spawn should succeed");

        let grandchild_pid = poll_for_pidfile(&pidfile, Duration::from_secs(5));
        let child_pid = guard.id().expect("child pid should be available");

        guard.kill_tree().await.expect("kill_tree should succeed");

        assert_process_gone(child_pid, Duration::from_secs(5), "child");
        assert_process_gone(grandchild_pid, Duration::from_secs(5), "grandchild");
    }

    #[tokio::test]
    async fn wait_timeout_kills_tree_and_returns_timeout_error() {
        let dir = tempdir().expect("tempdir");
        let pidfile = dir.path().join("grandchild.pid");
        let mut guard =
            ChildGuard::spawn(spawn_sleep_tree(&pidfile)).expect("spawn should succeed");

        let grandchild_pid = poll_for_pidfile(&pidfile, Duration::from_secs(5));
        let child_pid = guard.id().expect("child pid should be available");

        let err = guard
            .wait_timeout(Duration::from_millis(200))
            .await
            .expect_err("wait_timeout should return timeout error");
        assert_eq!(err.code, "timeout");

        assert_process_gone(child_pid, Duration::from_secs(5), "child");
        assert_process_gone(grandchild_pid, Duration::from_secs(5), "grandchild");
    }

    #[tokio::test]
    async fn wait_timeout_ok_and_kill_tree_idempotent() {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg("exit 0");
        let mut guard = ChildGuard::spawn(cmd).expect("spawn should succeed");

        let status = guard
            .wait_timeout(Duration::from_secs(5))
            .await
            .expect("wait_timeout should return exit status");
        assert!(status.success(), "child should exit successfully");

        guard
            .kill_tree()
            .await
            .expect("first kill_tree should succeed");
        guard
            .kill_tree()
            .await
            .expect("second kill_tree should be idempotent");
    }

    #[tokio::test]
    async fn drop_kills_child_tree() {
        let dir = tempdir().expect("tempdir");
        let pidfile = dir.path().join("grandchild.pid");
        let guard = ChildGuard::spawn(spawn_sleep_tree(&pidfile)).expect("spawn should succeed");

        let _grandchild_pid = poll_for_pidfile(&pidfile, Duration::from_secs(5));
        let child_pid = guard.id().expect("child pid should be available");

        drop(guard);

        assert_process_gone(child_pid, Duration::from_secs(5), "child");
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use std::fs;
    use std::path::Path;
    use std::process::Command as StdCommand;
    use std::thread;
    use std::time::Instant;
    use tempfile::tempdir;
    use tokio::process::Command;

    fn poll_for_pidfile(path: &Path, timeout: Duration) -> u32 {
        let start = Instant::now();
        loop {
            if let Ok(contents) = fs::read_to_string(path) {
                let trimmed = contents.trim().trim_start_matches('\u{feff}');
                if !trimmed.is_empty() {
                    return trimmed.parse::<u32>().expect("grandchild pid should parse");
                }
            }
            assert!(
                start.elapsed() < timeout,
                "timed out waiting for pidfile {}",
                path.display()
            );
            thread::sleep(Duration::from_millis(50));
        }
    }

    fn process_exists(pid: u32) -> bool {
        StdCommand::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                &format!(
                    "if (Get-Process -Id {pid} -ErrorAction SilentlyContinue) {{ exit 0 }} else {{ exit 1 }}"
                ),
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn assert_process_gone(pid: u32, timeout: Duration, label: &str) {
        let start = Instant::now();
        while process_exists(pid) {
            assert!(
                start.elapsed() < timeout,
                "{label} process {pid} still running after {:?}",
                timeout
            );
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn spawn_sleep_tree(pidfile: &Path) -> Command {
        // Spawn a grandchild via Start-Process and wait for it so the child stays alive.
        let script = format!(
            "$p = Start-Process -FilePath 'ping' -ArgumentList '-n','60','127.0.0.1' -PassThru; \
             $p.Id | Out-File -FilePath '{}' -Encoding ascii -NoNewline; \
             Wait-Process -Id $p.Id",
            pidfile.display()
        );
        let mut cmd = Command::new("powershell");
        cmd.args(["-NoProfile", "-Command", &script]);
        cmd
    }

    #[tokio::test]
    async fn kill_tree_terminates_child_and_grandchild() {
        let dir = tempdir().expect("tempdir");
        let pidfile = dir.path().join("grandchild.pid");
        let mut guard =
            ChildGuard::spawn(spawn_sleep_tree(&pidfile)).expect("spawn should succeed");

        let grandchild_pid = poll_for_pidfile(&pidfile, Duration::from_secs(5));
        let child_pid = guard.id().expect("child pid should be available");

        guard.kill_tree().await.expect("kill_tree should succeed");

        assert_process_gone(child_pid, Duration::from_secs(5), "child");
        assert_process_gone(grandchild_pid, Duration::from_secs(5), "grandchild");
    }

    #[tokio::test]
    async fn wait_timeout_kills_tree_and_returns_timeout_error() {
        let dir = tempdir().expect("tempdir");
        let pidfile = dir.path().join("grandchild.pid");
        let mut guard =
            ChildGuard::spawn(spawn_sleep_tree(&pidfile)).expect("spawn should succeed");

        let grandchild_pid = poll_for_pidfile(&pidfile, Duration::from_secs(5));
        let child_pid = guard.id().expect("child pid should be available");

        let err = guard
            .wait_timeout(Duration::from_millis(200))
            .await
            .expect_err("wait_timeout should return timeout error");
        assert_eq!(err.code, "timeout");

        assert_process_gone(child_pid, Duration::from_secs(5), "child");
        assert_process_gone(grandchild_pid, Duration::from_secs(5), "grandchild");
    }

    #[tokio::test]
    async fn wait_timeout_ok_and_kill_tree_idempotent() {
        let mut cmd = Command::new("powershell");
        cmd.args(["-NoProfile", "-Command", "exit 0"]);
        let mut guard = ChildGuard::spawn(cmd).expect("spawn should succeed");

        let status = guard
            .wait_timeout(Duration::from_secs(5))
            .await
            .expect("wait_timeout should return exit status");
        assert!(status.success(), "child should exit successfully");

        guard
            .kill_tree()
            .await
            .expect("first kill_tree should succeed");
        guard
            .kill_tree()
            .await
            .expect("second kill_tree should be idempotent");
    }

    #[tokio::test]
    async fn drop_kills_child_tree() {
        let dir = tempdir().expect("tempdir");
        let pidfile = dir.path().join("grandchild.pid");
        let guard = ChildGuard::spawn(spawn_sleep_tree(&pidfile)).expect("spawn should succeed");

        let _grandchild_pid = poll_for_pidfile(&pidfile, Duration::from_secs(5));
        let child_pid = guard.id().expect("child pid should be available");

        drop(guard);

        assert_process_gone(child_pid, Duration::from_secs(5), "child");
    }
}
