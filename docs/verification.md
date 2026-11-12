# Verification Record

Date: 2026-11-12

This record captures the evidence available for the OneGet desktop shell baseline. It separates local automated results from hosted CI, manual platform acceptance, and checks that have not been performed. It does not claim results that are still pending.

## Environment

| Component | Value |
| --- | --- |
| Host | macOS 27.3, arm64 |
| Node.js | 24.18.0 |
| npm | 11.16.0 |
| rustc | 1.99.1 |
| cargo | 1.99.1 |
| OpenSSL | 4.1.0 |

## Local automated evidence

| Check | Result | Detail |
| --- | --- | --- |
| `npm ci` | Passed | Locked install from the committed lockfile. |
| `npm run check:pins` | Passed | 26 direct npm entries are exact versions. |
| `npm run typecheck` | Passed | Strict TypeScript project build. |
| `npm run lint` | Passed | ESLint with React hooks and refresh rules. |
| `npm run test` | Passed | 20 Vitest tests. |
| `npm run build` | Passed | Production Vite build. |
| `npm run test:e2e` | Passed | 8 Playwright checks: 3 shell and design-system specs across light and dark projects. |
| `cargo check` | Passed | Locked Rust dependency graph. |
| `cargo fmt --check` | Passed | Formatting check. |
| `cargo clippy --all-targets -- -D warnings` | Passed | Warnings denied. |
| `cargo test` | Passed | 51 integration tests across 9 suites. |
| Tauri bundle | Passed | Application bundle built on macOS arm64. |

The Rust suites cover the following delivered behavior:

- capability create, write, sync, read, and atomic replace;
- traversal, symbolic-link, and junction escape rejection that leaves outside files untouched;
- adversarial filename tables for both targets, including normalization, reserved device names, trailing characters, control characters, and length limits;
- Ed25519 manifest acceptance for valid signatures and rejection of modified fields, reordered input, whitespace changes, wrong keys, malformed signatures and keys, non-object roots, duplicate keys, oversized input, and invalid JSON;
- process-group and Job Object termination for a child and grandchild, timeout handling, idempotent kills, and drop behavior;
- in-memory secret-store round-trip, missing keys, unavailable storage, empty values, idempotent delete, and redacted formatting;
- log redaction, JSON-line integrity, and size rotation;
- settings defaults, persistence, corrupt-file backup, future-schema read-only handling, unknown-field preservation, and write-failure recovery;
- deep-link shell contract parsing, accepted and rejected events, rejection of percent-decoded ASCII control characters, typed errors, autostart and notification service paths, secret round-trip behavior, shutdown logging, and the log-path contract.

## Self-signed certificate flow

A local macOS run created an RSA self-signed code-signing certificate in a temporary directory outside the repository, imported it into a temporary keychain, confirmed that the identity was discoverable, compiled a temporary Mach-O probe, signed it, and verified the signature with `codesign --verify --strict` plus identifier, authority, and non-ad-hoc checks. Cleanup removed the temporary keychain and certificate files. Runs with missing inputs exited non-zero. This flow is local verification only and is not release signing.

## Hosted CI evidence

The delivered GitHub Actions workflow ran on the authorized remote and passed.

| Field | Value |
| --- | --- |
| Workflow run | `ci #1`, run 43143112787 |
| URL | https://github.com/minbox-projects/one-get/actions/runs/43143112787 |
| Head SHA | `e6824885036a7de45255f5eac9805c22f830d8ed` |
| Conclusion | Success |

| Job | Result |
| --- | --- |
| frontend | Passed |
| rust gates (`macos-15`) | Passed |
| rust gates (`windows-2025`) | Passed |
| build (`aarch64-apple-darwin`) | Passed |
| build (`x86_64-apple-darwin`) | Passed |
| build (`x86_64-pc-windows-msvc`) | Passed |
| macOS signing dry-run | Passed |

| Artifact | Size | State |
| --- | --- | --- |
| `oneget-aarch64-apple-darwin` | 4,189,769 bytes | Uploaded, not expired |
| `oneget-x86_64-apple-darwin` | 4,266,731 bytes | Uploaded, not expired |
| `oneget-x86_64-pc-windows-msvc` | 2,688,038 bytes | Uploaded, not expired |

The run covers hosted CI gates, builds, and the signing dry-run only. It does not cover any manual platform acceptance item below.

## Manual acceptance status

All items below are pending and were not performed.

| Item | Status | Notes |
| --- | --- | --- |
| macOS arm64 build and launch | Pending | Confirm the main window and diagnostics surface. |
| macOS single instance | Pending | Launch twice and confirm one running instance. |
| macOS deep links | Pending | Open valid and invalid `oneget://` links against the built app. |
| macOS tray and window lifecycle | Pending | Confirm close hides the window and tray Show and Quit behave correctly. |
| macOS autostart | Pending | Enable and disable, inspect the LaunchAgent entry, restart, and confirm behavior. |
| macOS notifications | Pending | Grant and deny permission and send a test notification. |
| macOS credential round-trip | Pending | Run the keyring diagnostic against the real operating-system store. |
| macOS light and dark visual check | Pending | Inspect both appearance modes. |
| macOS startup network log | Pending | Confirm the startup path makes no remote requests. |
| macOS x86_64 launch | Pending | The hosted CI run uploads the x86_64 artifact; launch on Intel hardware remains pending. |
| Windows x64 install and launch | Pending | The hosted CI run builds the NSIS artifact; install and launch remain pending. |
| Windows platform checks | Pending | Repeat single-instance, deep-link, tray, autostart, notification, credential, visual, and network checks. |
| Windows notifications from installed build | Pending | Test from the installed NSIS build rather than the release binary. |

## Skipped and deferred checks

- Every manual acceptance item listed above.
- macOS x86_64 launch on Intel hardware.
- Windows install, launch, and all Windows platform checks.
- The startup network-log inspection and the manual light and dark visual review.
- macOS local signing trust-prompt behavior on the unsigned versus self-signed build.

Phase exit is not claimed. The baseline remains incomplete until the manual acceptance matrix produces recorded evidence, unless a later decision explicitly narrows the target matrix.

## Risk status

| Priority | Status | Detail |
| --- | --- | --- |
| P0 | No failure observed in automated evidence | No required behavior failed in the local macOS arm64 automated evidence or the hosted CI run. |
| P1 | Pending | Hosted CI is green; manual macOS arm64 acceptance, Windows install and platform checks, and the macOS x86_64 Intel launch remain pending. |
| P2 | Open with mitigation | Self-signed certificate trust prompts are expected locally; Windows notifications require the installed NSIS build; the Intel runner is available only until its announced retirement; unsigned local builds can change keychain and notification prompt behavior; macOS deep links require LaunchServices registration from a built app. |

The P0 statement is limited to automated evidence. It is not a phase-exit approval and does not cover the pending manual, Windows install, or Intel results.

## Phase 1: download engine and task store

Date: 2026-11-12

This section records the headless HTTP/HTTPS download engine and durable task store. The engine, store, command-line harness, local fixtures, and soak suite are implemented and exercised on macOS arm64. Implementation completion is not Phase 1 exit. Phase 1 exit stays blocked while any required Phase 0 manual item or the 24-hour soak is pending, unless the user records a dated explicit waiver. Nothing below records a pending gate as passed.

### Environment

| Component | Value |
| --- | --- |
| Host | macOS 27.3 (build 26C5035r), arm64 |
| Node.js | 24.18.0 |
| npm | 11.16.0 |
| rustc | 1.99.1 (58cf8f9c3 2026-10-13) |
| cargo | 1.99.1 (5f94df478 2026-08-27) |

### Automated commands and results

Test commands ran in `src-tauri/` against local ephemeral-port fixtures; the formatting, check, and lint gates ran in the same crate. Each row is an accepted implementation transcript.

| Command | Result |
| --- | --- |
| `cargo test --test store --locked` | Passed: 10 tests. |
| `cargo test --test engine_probe` | Passed: 12 tests. |
| `cargo test --test engine_download --test engine_segments --test platform_files` | Passed: 6, 6, and 5 tests. |
| `cargo test --test engine_queue` | Passed: 8 tests. |
| `cargo test --test engine_recovery` | Passed: 6 tests. |
| `cargo test --test engine_network` | Passed: 3 tests covering split and Range fallback. |
| `cargo test --test engine_download --test engine_network` | Passed: 9 and 13 tests. |
| `cargo test --test engine_network --test engine_redaction` | Passed: 23 and 1 tests. |
| `cargo test --test platform_files` | Passed: 7 tests. |
| `cargo test --test engine_large --test platform_files` | Passed: 4 and 7 tests; exit 0 (post-fix green transcript). |
| `cargo test --test soak` | Passed: 1 short smoke test; the 24-hour test is ignored. |
| `ONEGET_SOAK_SECONDS=2 cargo test --test soak -- --ignored --nocapture` | Passed: 1 iteration, 8 tasks, 7 completed, 1 cancelled, 0 failed, 1 retried, 17,825,792 bytes, integrity `ok`, 0 orphan parts, 0 unhandled panics. |
| `cargo fmt -- --check` | Passed: exit 0. |
| `cargo check --locked` | Passed: exit 0. |
| `cargo clippy --all-targets -- -D warnings` | Passed: exit 0. |

The 2-second ignored-path run is not 24-hour evidence. No public-network dependency is used; every network test reaches a local fixture, proxy, or cookie fixture.

### Acceptance criteria

Every criterion below is recorded with its status and the behavior test or record that carries it. All automated evidence is macOS arm64.

| Criterion | Status | Evidence |
| --- | --- | --- |
| AC-001 | Passed | This record lists the Phase 0 matrix and the soak as pending and blocks Phase 1 exit. |
| AC-002 | Passed | `engine_probe`: 206 with a valid Content-Range, 200 with Content-Length, and 200 without a length. |
| AC-003 | Passed | `engine_probe`: HTML rejection and allowance, 416 zero and nonzero totals, 401, 404, 429, and 500 mapping. |
| AC-004 | Passed | `engine_probe`: bounded chain, HTTPS-to-HTTP rejection, credentialed target rejection, cross-host header and cookie stripping. |
| AC-005 | Passed | `engine_probe` URL and filename cases; `engine_network` forbidden and CRLF header cases. |
| AC-006 | Passed | `engine_segments` planner matrix: totals from zero to 10 GiB and connection counts 1, 8, 64, 0, and 65. |
| AC-007 | Passed | `engine_network` dynamic split keeps ranges, split depth, and final hash. |
| AC-008 | Passed | `engine_download` 8 MiB eight-connection hash, part cleanup, and stored hash. |
| AC-009 | Passed | `engine_download` destination collision and zero-byte finalization. |
| AC-010 | Passed | `engine_download` idempotent finalize and crash before or after rename. |
| AC-011 | Passed | `store` schema 1, indexes, WAL, permissions, future-schema refusal, and migration rollback with backup. |
| AC-012 | Passed | `store` task and segment transitions, invalid ranges, and part preservation rules. |
| AC-013 | Passed | `engine_queue` FIFO at limits one and two, paused slot release, finalizing slot use, and live limit change. |
| AC-014 | Passed | `engine_queue` pause and resume progress, no rerequest, no-op and invalid-state cases. |
| AC-015 | Passed | `engine_recovery` changed ETag, Last-Modified, or size, and missing part. |
| AC-016 | Passed | `engine_recovery` harness kill and restart with bounded re-download and no orphans. |
| AC-017 | Passed | `engine_recovery` final-only, part-only, neither-file, and leftover cancelled-part cases. |
| AC-018 | Passed | `engine_queue` and `engine_recovery` cancel scope, repeated cancel, and finalization race. |
| AC-019 | Passed | `engine_network` fresh task falls back to one connection after a post-plan 200. |
| AC-020 | Passed | `engine_network` and `engine_segments` ignore-after-bytes failure with preserved part. |
| AC-021 | Passed | `engine_network` transient 500, integer `Retry-After`, HTTP-date fallback, and bounded persistent failure. |
| AC-022 | Passed | `engine_network` 401, 403, 404, invalid Content-Range, overlong body, truncated body, and timeout. |
| AC-023 | Passed | `engine_network` per-task and global limits within ten percent; unlimited task not throttled. |
| AC-024 | Passed | `engine_network` proxy basic authentication, wrong password, userinfo rejection, and credential absence. |
| AC-025 | Passed | `engine_network` host and path scope, expiry, secure-over-HTTP, cross-host redirect, and no cookie logging. |
| AC-026 | Passed | `engine_network` safe header reaches the fixture; forbidden and newline headers are rejected; no header value is persisted. |
| AC-027 | Passed | `engine_download` injected disk-full pause and resume with preserved progress. |
| AC-028 | Passed | `engine_network` truncated body retries from the persisted offset; persistent truncation stops. |
| AC-029 | Passed | `engine_download` HTML rejection and allowance; checksum mismatch preserves the part and writes no final file. |
| AC-030 | Passed (macOS, accepted result); Windows compatibility pending | `engine_large` 5 GiB sparse part file with head, middle, and tail writes plus 64-segment arithmetic above 4 GiB; the post-fix green transcript shows 4 `engine_large` and 7 `platform_files` tests passing with exit 0. |
| AC-031 | Passed | Local range fixture modes, proxy and cookie fixtures, harness commands, and the short soak smoke; no public host. |
| AC-032 | Passed | `engine_redaction` scans logs, the SQLite text dump, and user-facing errors for query tokens, cookies, proxy credentials, and header values; no telemetry destination. |
| AC-033 | Passed | This section, `MEMORY.md`, `docs/architecture.md`, the implemented architecture note triplet, and navigation validation record completion separately from exit. |

### AC-026 and AC-032 conflict resolution

The privacy requirement wins. Custom header VALUES are never persisted or logged. A safe header NAME may be persisted with an empty or redacted value so a fresh engine can see which safe headers were configured, but the value is not replayable after the in-memory session ends. Forbidden and CRLF-bearing headers are rejected before any request and are never stored. This keeps `tasks.headers_json` metadata-only and satisfies the secret scan.

### Evidence gaps

The retained intermediate transcript records an earlier revision where the 5 GiB sparse test failed on its middle-offset assertion; the test was then corrected to write at 4 GiB plus 256 MiB. The post-fix green transcript is preserved in the reports set and records `cargo test --test engine_large --test platform_files` in `src-tauri/` with exit 0, `engine_large` 4 passed, and `platform_files` 7 passed. The Windows x64 run is still required and is not inferred from macOS.

### Platform compatibility observations

| Target | Item | Status |
| --- | --- | --- |
| macOS arm64 | Sparse 5 GiB part file and offsets above 4 GiB | Proven; see AC-030. |
| Windows x64 | Offset arithmetic and sparse attempt or fallback | Pending; not run for this change and never recorded as a pass. |
| Windows x64 | Full Rust gates, short soak smoke, capability-scoped replace | Pending; no Windows run is claimed. |

### Phase 0 manual prerequisite status

The Phase 0 manual acceptance matrix above remains entirely pending: 13 items, none passed, and no dated waiver. Pending Phase 0 items block Phase 1 exit. This record does not restate them as passed.

### 24-hour soak status

Pending. Only the short smoke and the 2-second ignored-path check ran. The ignored 24-hour command is `ONEGET_SOAK_SECONDS=86400 cargo test --test soak -- --ignored --nocapture` in `src-tauri/`, or `bash scripts/soak-test.sh 86400` from the repository root. A short run is not 24-hour evidence. The required record must include start and end time, task counts, final hashes, database integrity, orphan-part check, and RSS at warm-up and at the end.

### Exit statement

The engine and store implementation, automated suites, harness, and soak harness are complete. Phase 1 exit is blocked. It requires a completed Phase 0 manual acceptance matrix or a dated explicit user waiver, plus a recorded 24-hour soak, before any exit claim. No P0 failure was observed in the macOS arm64 automated evidence, but this is not a phase-exit approval.
