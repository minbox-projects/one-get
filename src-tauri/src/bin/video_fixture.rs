//! Deterministic fixture process used by the video process tests.
//!
//! Each mode exercises one guarded-runner behavior with the standard library
//! only, so the same executable works on macOS and Windows.

use std::collections::BTreeMap;
use std::io::{self, Write};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

const FLOOD_BYTES: usize = 3 * 1024 * 1024;
const CHUNK: usize = 64 * 1024;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("probe-json") => probe_json(),
        Some("emit") => emit(&args[1..]),
        Some("progress") => progress(&args[1..]),
        Some("merge") => {
            println!("merge stage");
            flush_stdout();
        }
        Some("failure") => {
            eprintln!("fixture failure");
            std::process::exit(1);
        }
        Some("hang") => hang(),
        Some("hang-after-line") => {
            println!("progress first=1");
            flush_stdout();
            hang();
        }
        Some("flood") => flood(),
        Some("flood-and-hang") => {
            flood();
            hang();
        }
        Some("grandchild") => grandchild(),
        Some("sleep-child") => hang(),
        Some("env") => environment(),
        Some("echo-args") => echo_args(&args[1..]),
        Some(other) => {
            eprintln!("video_fixture: unknown mode {other}");
            std::process::exit(2);
        }
        None => {
            eprintln!("video_fixture: missing mode");
            std::process::exit(2);
        }
    }
}

fn probe_json() {
    let document = serde_json::json!({
        "id": "fixture-video",
        "title": "Fixture Video",
        "duration": 123.5,
        "uploader": "Fixture",
        "is_live": false,
        "formats": [
            {"format_id": "137", "height": 1080, "ext": "mp4", "vcodec": "avc1.640028", "acodec": "none"},
            {"format_id": "140", "vcodec": "none", "acodec": "mp4a.40.2", "ext": "m4a"}
        ]
    });
    println!("{document}");
}

fn emit(args: &[String]) {
    let path = args
        .iter()
        .position(|argument| argument == "--path")
        .and_then(|index| args.get(index + 1));
    let Some(path) = path else {
        eprintln!("video_fixture: emit requires --path <file>");
        std::process::exit(2);
    };
    match std::fs::read(path) {
        Ok(body) => {
            let mut stdout = io::stdout().lock();
            let _ = stdout.write_all(&body);
            let _ = stdout.flush();
        }
        Err(error) => {
            eprintln!("video_fixture: cannot read {path}: {error}");
            std::process::exit(2);
        }
    }
}

fn progress(args: &[String]) {
    let lines = args
        .iter()
        .position(|argument| argument == "--lines")
        .and_then(|index| args.get(index + 1))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(3);
    let mut stdout = io::stdout().lock();
    for index in 0..lines {
        let _ = writeln!(stdout, "progress downloaded={} total=100", index + 1);
    }
    let _ = stdout.flush();
}

fn flood() {
    {
        let mut stdout = io::stdout().lock();
        write_bytes(&mut stdout, FLOOD_BYTES);
    }
    {
        let mut stderr = io::stderr().lock();
        write_bytes(&mut stderr, FLOOD_BYTES);
    }
}

fn write_bytes<W: Write>(writer: &mut W, total: usize) {
    let block = vec![b'x'; CHUNK];
    let mut written = 0;
    while written < total {
        let remaining = total - written;
        let length = remaining.min(CHUNK);
        let _ = writer.write_all(&block[..length]);
        written += length;
    }
    let _ = writer.flush();
}

fn grandchild() {
    let executable = std::env::current_exe().expect("current executable resolves");
    let mut child = Command::new(executable)
        .arg("sleep-child")
        .stdin(Stdio::null())
        .spawn()
        .expect("sleep-child spawns");
    println!("child={}", child.id());
    flush_stdout();
    // Wait for the grandchild so the parent stays alive until the whole tree is
    // killed; the grandchild itself never exits on its own.
    let _ = child.wait();
}

fn environment() {
    let mut environment = BTreeMap::new();
    for (key, value) in std::env::vars_os() {
        environment.insert(
            key.to_string_lossy().into_owned(),
            value.to_string_lossy().into_owned(),
        );
    }
    let document = serde_json::to_string(&environment).expect("the environment serializes");
    println!("{document}");
}

fn echo_args(args: &[String]) {
    for argument in args {
        println!("{argument}");
    }
}

fn hang() -> ! {
    loop {
        thread::sleep(Duration::from_secs(1));
    }
}

fn flush_stdout() {
    let _ = io::stdout().flush();
}
