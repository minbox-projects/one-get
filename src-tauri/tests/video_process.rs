//! Behavior tests for the guarded video process runner and its deterministic fixture binary.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use oneget_lib::video::error::VideoError;
use oneget_lib::video::process::{
    run_download, run_probe, ProcessOutput, ProcessSpec, TimeoutPolicy,
};

const MIB: usize = 1024 * 1024;
const COMBINED_OUTPUT_CAP: usize = 2 * MIB;
const STREAM_TAIL: usize = 8 * 1024;

fn fixture_spec(args: &[&str]) -> ProcessSpec {
    ProcessSpec {
        program: PathBuf::from(env!("CARGO_BIN_EXE_video_fixture")),
        args: args.iter().map(|argument| argument.to_string()).collect(),
        cwd: None,
        env: Vec::new(),
    }
}

fn expect_video_error(result: Result<ProcessOutput, VideoError>, context: &str) -> VideoError {
    match result {
        Ok(_) => panic!("{context}: the runner unexpectedly succeeded"),
        Err(error) => error,
    }
}

fn captured_len(output: &ProcessOutput) -> usize {
    output.stdout.len() + output.stderr.len()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_times_out_and_kills_the_tree() {
    let request = fixture_spec(&["hang"]);
    let policy = TimeoutPolicy {
        probe: Duration::from_millis(150),
        ..TimeoutPolicy::default()
    };

    let started = Instant::now();
    let error = expect_video_error(
        run_probe(&request, &policy).await,
        "a probe that never exits must time out",
    );

    assert_eq!(
        error.code(),
        "VIDEO_TIMEOUT",
        "a hung probe must report the stable timeout code"
    );
    assert!(
        !error.message().is_empty(),
        "a timeout must carry a user-safe message"
    );
    assert!(
        format!("{error}").contains("VIDEO_TIMEOUT"),
        "Display must expose the stable code"
    );
    let rendered: &dyn std::error::Error = &error;
    assert!(rendered.to_string().contains("VIDEO_TIMEOUT"));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the timeout must return promptly instead of waiting for the child: {:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_output_flood_hits_the_cap_without_blocking() {
    let request = fixture_spec(&["flood"]);
    let policy = TimeoutPolicy {
        probe: Duration::from_secs(5),
        ..TimeoutPolicy::default()
    };

    let started = Instant::now();
    match run_probe(&request, &policy).await {
        Ok(output) => {
            assert!(
                output.truncated,
                "a completed probe above the cap must return truncated evidence"
            );
            assert!(
                captured_len(&output) <= COMBINED_OUTPUT_CAP + 2 * STREAM_TAIL,
                "captured output must stay near the 2 MiB cap plus tails, got {} bytes",
                captured_len(&output)
            );
            panic!("a completed probe above the output cap must return VIDEO_OUTPUT_LIMIT");
        }
        Err(error) => {
            assert_eq!(
                error.code(),
                "VIDEO_OUTPUT_LIMIT",
                "an output flood that completes must report the stable output-limit code"
            );
        }
    }

    assert!(
        started.elapsed() < Duration::from_secs(5),
        "both pipes must drain concurrently instead of blocking: {:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn probe_timeout_wins_over_a_simultaneous_flood() {
    let request = fixture_spec(&["flood-and-hang"]);
    let policy = TimeoutPolicy {
        probe: Duration::from_millis(200),
        ..TimeoutPolicy::default()
    };

    let started = Instant::now();
    match run_probe(&request, &policy).await {
        Ok(output) => panic!(
            "a timeout must win over a simultaneous output flood, got truncated={}",
            output.truncated
        ),
        Err(error) => {
            assert_eq!(
                error.code(),
                "VIDEO_TIMEOUT",
                "a process that floods and hangs must report the timeout code"
            );
        }
    }

    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the flooded child must be killed at the timeout instead of hanging: {:?}",
        started.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn download_runner_drains_both_pipes_and_truncates() {
    let request = fixture_spec(&["flood"]);
    let policy = TimeoutPolicy {
        startup: Duration::from_secs(5),
        inactivity: Duration::from_secs(5),
        ..TimeoutPolicy::default()
    };

    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed = Arc::clone(&lines);
    let started = Instant::now();
    let output = match run_download(&request, &policy, move |line| {
        observed.lock().expect("line lock").push(line.to_string());
    })
    .await
    {
        Ok(output) => output,
        Err(error) => panic!("a flooding download must still complete: {error}"),
    };

    assert!(
        output.status.success(),
        "the flooding fixture must exit successfully"
    );
    assert!(
        output.truncated,
        "output above the combined cap must be reported as truncated"
    );
    assert!(!output.stdout.is_empty(), "the stdout pipe must be drained");
    assert!(!output.stderr.is_empty(), "the stderr pipe must be drained");
    assert!(
        captured_len(&output) <= COMBINED_OUTPUT_CAP + 2 * STREAM_TAIL,
        "captured stdout and stderr must stay near the 2 MiB cap plus tails, got {} bytes",
        captured_len(&output)
    );
    assert!(
        output.stdout.len() <= COMBINED_OUTPUT_CAP + STREAM_TAIL,
        "stdout capture must stay bounded"
    );
    assert!(
        output.stderr.len() <= COMBINED_OUTPUT_CAP + STREAM_TAIL,
        "stderr capture must stay bounded"
    );
    assert!(
        output.elapsed < Duration::from_secs(5),
        "the runner must not stall on a full pipe: {:?}",
        output.elapsed
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the callback path must not block the runner: {:?}",
        started.elapsed()
    );
    assert!(
        lines.lock().expect("line lock").len() <= output.stdout.len() + 1,
        "the line callback must stay bounded by the captured stdout"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn download_startup_and_inactivity_timeouts_are_distinct() {
    let defaults = TimeoutPolicy::default();
    assert_eq!(defaults.probe, Duration::from_secs(60));
    assert_eq!(defaults.startup, Duration::from_secs(30));
    assert_eq!(defaults.inactivity, Duration::from_secs(120));
    assert_eq!(defaults.shutdown, Duration::from_secs(5));

    let silent_request = fixture_spec(&["hang"]);
    let startup_policy = TimeoutPolicy {
        startup: Duration::from_millis(150),
        inactivity: Duration::from_secs(5),
        ..TimeoutPolicy::default()
    };
    let startup_lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let startup_observed = Arc::clone(&startup_lines);
    let startup_started = Instant::now();
    let startup_error = expect_video_error(
        run_download(&silent_request, &startup_policy, move |line| {
            startup_observed
                .lock()
                .expect("line lock")
                .push(line.to_string());
        })
        .await,
        "a download that never prints a first line must hit the startup timeout",
    );

    assert_eq!(startup_error.code(), "VIDEO_TIMEOUT");
    assert!(
        startup_started.elapsed() < Duration::from_secs(5),
        "the startup timeout must return promptly: {:?}",
        startup_started.elapsed()
    );
    assert!(
        startup_lines.lock().expect("line lock").is_empty(),
        "no line may be delivered before the startup timeout"
    );

    let late_request = fixture_spec(&["hang-after-line"]);
    let inactivity_policy = TimeoutPolicy {
        startup: Duration::from_secs(5),
        inactivity: Duration::from_millis(150),
        ..TimeoutPolicy::default()
    };
    let inactivity_lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let inactivity_observed = Arc::clone(&inactivity_lines);
    let inactivity_started = Instant::now();
    let inactivity_error = expect_video_error(
        run_download(&late_request, &inactivity_policy, move |line| {
            inactivity_observed
                .lock()
                .expect("line lock")
                .push(line.to_string());
        })
        .await,
        "a download that prints one line and then stalls must hit the inactivity timeout",
    );

    assert_eq!(inactivity_error.code(), "VIDEO_TIMEOUT");
    assert!(
        inactivity_started.elapsed() < Duration::from_secs(5),
        "the inactivity timeout must return promptly: {:?}",
        inactivity_started.elapsed()
    );
    assert_eq!(
        inactivity_lines.lock().expect("line lock").len(),
        1,
        "the first line must be delivered before the inactivity timeout"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn grandchild_is_reaped_after_timeout() {
    // A probe timeout error carries no captured bytes, so the download runner's
    // line callback observes the grandchild PID while the same whole-tree
    // timeout path reaps it.
    let request = fixture_spec(&["grandchild"]);
    let policy = TimeoutPolicy {
        probe: Duration::from_millis(300),
        startup: Duration::from_secs(5),
        inactivity: Duration::from_millis(300),
        ..TimeoutPolicy::default()
    };

    let lines = Arc::new(Mutex::new(Vec::<String>::new()));
    let observed = Arc::clone(&lines);
    let started = Instant::now();
    let error = expect_video_error(
        run_download(&request, &policy, move |line| {
            observed.lock().expect("line lock").push(line.to_string());
        })
        .await,
        "a process tree that prints a child PID and then hangs must time out",
    );

    assert_eq!(error.code(), "VIDEO_TIMEOUT");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the whole-tree kill must return promptly: {:?}",
        started.elapsed()
    );

    let child_pid = lines
        .lock()
        .expect("line lock")
        .iter()
        .find_map(|line| {
            line.trim()
                .strip_prefix("child=")
                .and_then(|value| value.parse::<u32>().ok())
        })
        .expect("grandchild mode must print child=<pid>");

    #[cfg(unix)]
    {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let alive = std::process::Command::new("kill")
                .arg("-0")
                .arg(child_pid.to_string())
                .status()
                .expect("kill -0 must run")
                .success();
            if !alive {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "grandchild {child_pid} survived the whole-tree timeout kill"
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    #[cfg(not(unix))]
    {
        let _ = child_pid;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn environment_is_minimal_and_allowlisted() {
    const PARENT_ONLY: &str = "ONEGET_VIDEO_PARENT_ONLY_CANARY";
    const ALLOWED_KEY: &str = "ONEGET_VIDEO_ALLOWED_PAIR";
    const ALLOWED_VALUE: &str = "allowed-value";

    std::env::set_var(PARENT_ONLY, "parent-secret");

    let mut request = fixture_spec(&["env"]);
    request.env = vec![(ALLOWED_KEY.to_string(), ALLOWED_VALUE.to_string())];
    let output = match run_probe(&request, &TimeoutPolicy::default()).await {
        Ok(output) => output,
        Err(error) => panic!("the environment fixture must complete: {error}"),
    };
    std::env::remove_var(PARENT_ONLY);

    assert!(
        output.status.success(),
        "the environment fixture must exit successfully"
    );
    let document: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("env mode must print one JSON object");
    let environment = document
        .as_object()
        .expect("the process environment must serialize as a JSON object");

    assert_eq!(
        environment
            .get(ALLOWED_KEY)
            .and_then(|value| value.as_str()),
        Some(ALLOWED_VALUE),
        "the supplied environment pair must reach the child"
    );
    assert!(
        !environment.contains_key(PARENT_ONLY),
        "an inherited non-allowlisted variable must not reach the child"
    );

    #[cfg(unix)]
    {
        let parent_path = std::env::var("PATH").expect("PATH must be set in the test environment");
        assert_eq!(
            environment.get("PATH").and_then(|value| value.as_str()),
            Some(parent_path.as_str()),
            "PATH must be allowlisted for the child"
        );

        if let Ok(home) = std::env::var("HOME") {
            assert_eq!(
                environment.get("HOME").and_then(|value| value.as_str()),
                Some(home.as_str()),
                "HOME must be allowlisted for the child"
            );
        }
        if let Ok(tmpdir) = std::env::var("TMPDIR") {
            assert_eq!(
                environment.get("TMPDIR").and_then(|value| value.as_str()),
                Some(tmpdir.as_str()),
                "TMPDIR must be allowlisted for the child"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn arguments_are_forwarded_without_a_shell() {
    let payload = [
        "with space",
        "double\"quote",
        "single'quote",
        "semi;colon",
        "$(echo not-a-shell)",
        "pipe|and&",
    ];
    let mut arguments = vec!["echo-args"];
    arguments.extend(payload);

    let request = fixture_spec(&arguments);
    let output = match run_probe(&request, &TimeoutPolicy::default()).await {
        Ok(output) => output,
        Err(error) => panic!("the argument fixture must complete: {error}"),
    };

    assert!(
        output.status.success(),
        "the argument fixture must exit successfully"
    );
    let stdout = String::from_utf8(output.stdout).expect("echo-args output must be UTF-8");
    let lines: Vec<&str> = stdout.lines().collect();
    let expected: Vec<&str> = payload.to_vec();

    assert!(
        lines.ends_with(&expected),
        "each argument must be forwarded verbatim, got {lines:?}"
    );
    assert!(
        lines.len() == expected.len()
            || (lines.len() == expected.len() + 1 && lines[0] == "echo-args"),
        "no shell must split, substitute, or combine the forwarded arguments, got {lines:?}"
    );
}
