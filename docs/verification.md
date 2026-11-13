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
| P2 | Open with mitigation | Self-signed certificate trust prompts are expected locally; Windows notifications require the installed NSIS build; the Intel runner is available only until its announced retirement; unsigned local builds can change keychain and notification prompt behavior; macOS deep links require LaunchServices registration from a built app; the context indexer discovers only TypeScript, so the Rust engine, store, and harness navigation entries are hand-maintained and a rebuild would drop them. |

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
| Repair green transcript (full suite) | Passed: 143 tests across 19 suites with 1 ignored 24-hour soak, exit 0, and `fmt=0 clippy=0 check=0` from `repair-green.log`. New tests cover a missing cookie file, unavailable proxy credentials, cancel during finalization and the finalization race, cross-engine pause and cancel, every harness command, and filename traversal rejection. |

The 2-second ignored-path run is not 24-hour evidence. No public-network dependency is used; every network test reaches a local fixture, proxy, or cookie fixture.

Dependency compatibility: `rand` 0.10.3 and `bytes` 1.12.1 are exact direct pins in `src-tauri/Cargo.toml` (`=0.10.3` and `=1.12.1`) resolved through the committed `Cargo.lock`, and every Rust gate runs with `--locked`. `rand` supplies the full-jitter retry delay and `bytes` supplies the streamed chunk type; neither is an accidental transitive addition.

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
| AC-015 | Passed | `engine_recovery` changed ETag, Last-Modified, or size and missing part; `resume_with_missing_cookie_file_fails_with_cookie_file_error_and_keeps_part` and `engine_redaction` `resume_without_proxy_credentials_fails_with_proxy_auth_and_keeps_part` fail with their typed codes and keep the part. |
| AC-016 | Passed | `engine_recovery` harness kill and restart with bounded re-download and no orphans. |
| AC-017 | Passed | `engine_recovery` final-only, part-only, neither-file, and leftover cancelled-part cases. |
| AC-018 | Passed | `engine_queue` and `engine_recovery` cancel scope, repeated cancel, `cancel_during_finalization_removes_part_without_invalid_state`, and `cancel_racing_finalization_ends_completed_or_cancelled_only`, whose race outcomes are only `completed` or `cancelled`. |
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
| AC-031 | Passed | Local range fixture modes, proxy and cookie fixtures, all five harness commands (`run`, `status`, `pause`, `resume`, `cancel`) through `harness_status_pause_resume_cancel_controls_persist_transitions`, cross-engine pause and cancel, and the short soak smoke; no public host. |
| AC-032 | Passed | `engine_redaction` scans logs, the SQLite text dump, and user-facing errors for query tokens, cookies, proxy credentials, and header values; no telemetry destination. |
| AC-033 | Passed | This section, `MEMORY.md`, `docs/architecture.md`, the implemented architecture note triplet, and the validated `engine`, `store`, and `engine-harness` navigation entries record completion separately from exit. The context indexer discovers only TypeScript files, so the Rust entries are maintained by hand and a future `context rebuild` would drop them, requiring them to be reapplied. |

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

## Phase 2: desktop download UI

Date: 2026-11-13

This section records the delivered task-first desktop download interface, its Tauri command surface and engine service, per-task destinations, deletion and open actions, settings schema 2, bilingual interface, virtualization, and mock-IPC browser layer. The automated evidence below was captured after the cycle-2 repair at the current delivery head, before this documentation-only change, which adds no product behavior. Implementation completion and automated green results are not Phase 2 exit.

### Environment

| Component | Value |
| --- | --- |
| Host | macOS 27.3, arm64 |
| Node.js | 24.18.0 |
| npm | 11.16.0 |
| rustc | 1.99.1 |
| cargo | 1.99.1 |

### Automated commands and results

Frontend commands ran from the repository root; Rust commands ran in `src-tauri/`.

| Command | Result |
| --- | --- |
| `npm run check:pins` | Passed: 27 direct npm entries are exact versions. |
| `npm run typecheck` | Passed: strict TypeScript project build. |
| `npm run lint` | Passed: ESLint with React hooks and refresh rules. |
| `npm run test` | Passed: 33 Vitest files, 268 tests. |
| `npm run build` | Passed: production Vite build. |
| `npm run test:e2e` | Passed: 30 Playwright tests across the chromium-light and chromium-dark projects. |
| `cargo fmt -- --check` | Passed: exit 0. |
| `cargo check --locked` | Passed: exit 0. |
| `cargo clippy --all-targets -- -D warnings` | Passed: exit 0. |
| `cargo test --locked` | Passed: 176 tests, 1 ignored long soak. |

The Rust suites report: config 10, downloads_commands 11, engine_download 10, engine_large 4, engine_network 23, engine_probe 12, engine_queue 21, engine_recovery 9, engine_redaction 2, engine_segments 6, logging_redaction 2, manifest 13, platform_filename 2, platform_files 7, platform_opener 2, platform_paths 3, platform_process 4, platform_secrets 6, shell_contract 13, the short soak smoke 1, and store 15.

Automated layers:

- Vitest covers components, typed IPC wrappers, validation, i18n, engine-error localization, polling, and 10,000-row virtualization.
- Rust integration tests cover the download commands, settings schema 2, store migration, destinations and final URLs, conflict-safe part names, record and file deletion, the platform opener, the shell contract, and the retained engine suites.
- Playwright runs against deterministic mock IPC in `e2e/support/mock-ipc.ts`; it covers new download, controls, deletion, settings, language switching, and 10,000-row scrolling without any public-network dependency.
- The hosted CI workflow was not run again for this revision. The earlier shell-baseline run remains the only hosted evidence.

### Repaired delivery facts

The initial review found blockers, and the repair landed before this documentation change. The verified behavior now includes:

- Task-store schema 2 also has nullable `final_url`. The worker persists the post-redirect URL through the same minimizer, so userinfo, query, and fragment are absent, and `downloads_list` exposes it as `finalUrl` for redacted details.
- Same destination and filename tasks are safe. The normal part name is `<filename>.oneget.part`; only on a conflict does the engine use `<filename>.<first 8 of task id>.oneget.part`. The chosen name is persisted in `part_path` and drives download, resume, finalize, cancel, recovery, delete, and existence checks, with a legacy-name fallback. Final unique-rename behavior is unchanged.
- The engine service runs slow probes concurrently with the request and run loops, so a pending probe does not stall downloads or listings. Startup recovery completes before the service starts, and paused-task revalidation is guarded to run once per engine instance rather than on every run-loop restart.
- `@tauri-apps/plugin-dialog` 2.8.4 provides the localized directory picker in the new and batch dialogs, and `INVALID_DESTINATION` surfaces as a destination-specific actionable error.
- Stable engine codes are localized through `src/features/downloads/errorMessages.ts`; `CHECKSUM_MISMATCH` tells the user to retry only with the correct expected SHA-256 or without it. Success actions show localized toasts. Segment ranges are half-open, and connection and rate inputs enforce the `u32` connection bound and a positive rate limit capped at `Number.MAX_SAFE_INTEGER`.
- The diagnostics panel remains a retained, tested module that the task-first shell does not mount; the Toast primitive is mounted.

Cycle-2 hardening makes part naming safe across engine instances:

- Conflict detection compares the effective destination, so a version-1 row without a stored destination falls back to the engine default directory.
- Any same-filename task that is not completed or cancelled conflicts, as does a completed or cancelled task whose part file still exists.
- The conflict check, part-name choice, and task insert run in one `BEGIN IMMEDIATE` transaction, so two engines cannot both claim `<filename>.oneget.part`.
- Rate-limit inputs in the new, batch, and settings forms are capped at `Number.MAX_SAFE_INTEGER`, the largest value the IPC number boundary carries without rounding.

The automated counts in this section are the validated cycle-2 post-repair transcript; they include the collision and rate-limit boundary tests added by that repair.

One earlier full-suite run failed the short soak under transient infrastructure contention. The isolated `cargo test --test soak` run and the following full `cargo test --locked` run passed. This record reports that early failure truthfully as infrastructure contention, not as a product failure, and does not present the earlier run as green at the time it occurred.

### Acceptance criteria

| Criterion | Status | Evidence |
| --- | --- | --- |
| AC-001 | Passed (automated) | Component tests render the sidebar, toolbar, list, details, status bar, and empty state. |
| AC-002 | Passed (automated) | Component tests cover sidebar filtering and matching counts. |
| AC-003 | Passed (automated) | Task-row component tests cover known and unknown metadata rendering. |
| AC-004 | Passed (automated) | Virtualization component tests mount a bounded row set and reach row 10,000. |
| AC-005 | Passed (automated) | Filter and sort tests cover creation-time default, fixed state order, and clearing filters. |
| AC-006 | Passed (automated) | New-download component tests and the Playwright add flow create and list a task. |
| AC-007 | Passed (automated) | Validation tests reject every listed URL, filename, connection, rate, checksum, and header boundary. |
| AC-008 | Passed (automated) | Probe tests cover success, unknown length, ignored Range, HTML allowance, and failure. |
| AC-009 | Passed (automated) | Destination tests exercise the `@tauri-apps/plugin-dialog` picker, accept writable directories, reject invalid ones, and show the localized `INVALID_DESTINATION` action error instead of a generic failure. |
| AC-010 | Passed (automated) | Batch tests report line errors, skip in-batch duplicates, and add valid URLs. |
| AC-011 | Passed (automated) | Batch tests apply shared destination, connections, rate, and headers to every accepted task. |
| AC-012 | Passed (automated) | Control tests cover state-aware enablement and typed errors for every task state. |
| AC-013 | Passed (automated) | Control and Rust deletion tests prove part-only cancel, retry preservation, and the active-task precondition. |
| AC-014 | Passed (automated) | Bulk-action tests update eligible tasks and observe the polled state within one interval. |
| AC-015 | Passed (automated) | Record-deletion tests keep the completed final file and remove only the record and part. |
| AC-016 | Passed (automated) | File-deletion tests require confirmation, remove only the completed final file and record, and keep the record on a missing-file or permission failure; same-name tasks keep independent recorded parts. |
| AC-017 | Passed (automated) | Details tests cover redacted stored and post-redirect `finalUrl`, directory, sizes, speed, ETA, connections, timestamps, localized errors, and segments; a one-byte half-open segment renders as complete. |
| AC-018 | Passed (automated) | Open and copy tests cover the platform opener, missing targets, and redacted stored URLs. |
| AC-019 | Passed (automated) | Settings tests cover save, reopen persistence, the read-only network placeholder, and unknown-field preservation. |
| AC-020 | Passed (automated) | Config tests cover version 1 migration, corrupt backup recovery, and future-schema read-only behavior. |
| AC-021 | Passed (automated) | Settings tests cover immediate language and theme changes and documented captured-at-start limits. |
| AC-022 | Passed (automated) | Service and command tests cover startup recovery, task listing, live engine limits, and graceful shutdown. |
| AC-023 | Passed (automated) | Store and engine tests prove persisted destinations survive restart and invalid destinations fail with typed errors. |
| AC-024 | Passed (automated) | Polling tests prove a one-second visible bound, hidden-window pause, and bounded list refresh. |
| AC-025 | Passed (automated) | Wrapper and fallback tests cover a missing runtime and rejected IPC without exposing internals. |
| AC-026 | Passed (automated) | The i18n audit covers both dictionaries, system detection, immediate switching, and locale formatting. |
| AC-027 | Passed (automated) | Component and Playwright keyboard tests cover shortcuts, focus trap and restore, and accessible names. |
| AC-028 | Passed (automated) | State tests cover loading, empty, filtered-empty, retryable error, confirmations, and localized success toasts; stable engine codes, including `CHECKSUM_MISMATCH`, render through localized dictionary messages rather than raw prose. |
| AC-029 | Passed | This section lists the automated layers, manual status, pending gates, and blocked exit. |
| AC-030 | Partial | Vitest, Rust, and mock-IPC Playwright layers passed; the manual real-app local HTTP download is pending. |

Component and mock-IPC results prove the interface behavior under deterministic mocks. They do not replace a manual real-application download against a local HTTP server.

### Pending gates and manual acceptance

| Gate | Status | Detail |
| --- | --- | --- |
| Phase 0 manual acceptance matrix | Pending | All 13 baseline items remain pending; no dated waiver exists. |
| Phase 1 24-hour soak | Pending | Only the short smoke and the 2-second ignored-path check ran; neither is 24-hour evidence. |
| Phase 1 Windows x64 compatibility | Pending | No Windows run of the engine, store, migration, or opener suites is claimed. |
| Manual real-app local HTTP download | Pending | Not performed at this revision. |
| Manual macOS acceptance | Pending | Launch, add, controls, details, open, delete, settings, language, and visual checks remain pending. |
| Manual Windows acceptance | Pending | Install, launch, download, opener, deletion, settings, and platform checks remain pending. |
| Design review | blockers repaired; first recheck complete; cycle-2 repair applied and verified; final target recheck pending | The full review found defects. The first aggregate repair and targeted recheck completed: Spec passed, and Standards closed four error findings and two advisories while flagging one residual advisory and note consistency. The cycle-2 repair is applied and verified, and the final targeted recheck at the next committed head is not yet recorded. |

The Phase 0 matrix and the Phase 1 soak and Windows gates block Phase 2 exit unless the user records a dated explicit waiver. The manual real-application download and the final targeted design-review recheck are also outstanding. No pending item above is recorded as passed.

### Documentation and navigation evidence

The implemented architecture note triplet records the repaired delivery facts and the cycle-2 collision hardening, is paired, and passes both filesystem and baseline validation. The active shell-baseline and engine notes remain active and linked; the UI record partially supersedes their earlier surface, part-name, and schema-version statements while retaining the process, platform, engine, and store invariants they still own, and both older notes now qualify their superseded settings, part-name, and schema statements and point to the current record. The navigation index was rebuilt for the new TypeScript modules and symbols, the hand-maintained Rust entries were reapplied, the Markdown view was regenerated from the JSON authority, and the full context validation passes. The context indexer discovers only TypeScript modules, so the Rust `engine`, `store`, `commands`, and `engine-harness` entries remain hand-maintained and a rebuild without reapplication drops them.

### Exit statement

Phase 2 exit is blocked. It requires the Phase 0 manual matrix and the Phase 1 24-hour soak and Windows compatibility records, or a dated explicit user waiver, plus a recorded manual real-application local HTTP download and a completed design review with its final targeted recheck recorded. The cycle-2 post-repair automated evidence is green, but it is not a phase-exit approval.
