# Project memory

This file records the current standards and behavior of the OneGet desktop application, its download engine, and its durable task store. Agent Notes record why decisions were made; keep both consistent in the same change.

## Stack and pins

- Node.js 24 (`>=24 <25`) with a committed `package-lock.json`; install with `npm ci`.
- React 19.3.1, React DOM 19.3.1, TypeScript 6.0.3, Vite 8.3.8, Tailwind 4.3.5, Vitest 5.1.0, Playwright 1.65.0, and ESLint 10.14.0.
- Tauri 2.12.5 with `@tauri-apps/api` and CLI 2.12.4; Rust edition 2021 on stable, with committed `src-tauri/Cargo.lock`.
- Rust boundaries: cap-std 4.0.3, keyring 4.2.0, ed25519-dalek 3.0.0, process-wrap 10.0.1, secrecy 0.10.3, zeroize 1.9.1, unicode-normalization 0.1, url 2.5.8.
- Engine additions: rusqlite 0.42.0 (`bundled`, `backup`), reqwest 0.13.5 (`rustls`, `stream`), sha2 0.10.9, uuid 1.29.2 (`v4`), rand 0.10.3, bytes 1.12.1, futures-util 0.3.34, percent-encoding 2.3.2, and Windows-only windows-sys 0.61.2.
- Every direct npm dependency is an exact version; `npm run check:pins` enforces this. Use `cargo --locked` for Rust gates.

## Repository and module boundaries

- `src/` is the React UI: shell, error boundary, design-system primitives, diagnostics, typed IPC wrapper, deep-link subscription, and shared types.
- `src-tauri/src/commands/` is the Tauri command surface plus directly testable service functions.
- `src-tauri/src/engine/` owns the headless probe, safe HTTP transport, planner, part file, worker pool, scheduler, retry, rate limiting, cookies, state machines, and typed engine errors.
- `src-tauri/src/store/` owns the SQLite connection, schema migration, task and segment records, and progress persistence.
- `src-tauri/src/bin/engine_harness.rs` is the command-line harness (`run`, `status`, `pause`, `resume`, `cancel`) for local fixture checks and crash recovery.
- `src-tauri/tests/support/` holds the deterministic local range, proxy, cookie, fault, and fixture helpers; `scripts/soak-test.sh` wraps the ignored long soak.
- `src-tauri/src/platform/` owns all target-specific behavior: paths, filenames, capability files, processes, secrets, deep links, tray, autostart, and notifications.
- `src-tauri/src/kernel/` owns canonical JSON and Ed25519 manifest verification only.
- `src-tauri/src/config.rs`, `logging/`, and `error.rs` own settings, JSON-line logs, and typed errors.
- `src-tauri/tests/` holds the Rust integration suites for the shell, store, engine, and soak; `e2e/` holds Playwright checks; `scripts/` holds the pin check, the soak wrapper, and local macOS signing helpers.
- `localdocs/` is ignored local reference material, not shipped product documentation.

## Platform-layer rule

Target-specific branches belong only in `src-tauri/src/platform/**` and platform-specific Tauri configuration. Shared modules call platform interfaces and never branch on the target operating system. New platform behavior needs an interface in the platform layer plus a test in the corresponding suite.

## Download engine and task store

- Entry points: `Engine` in `src-tauri/src/engine/scheduler.rs` (`new`, `add_task`, `run`, `pause`, `resume`, `cancel`, `retry`, `recover`, `set_max_concurrent`, `set_proxy`, `set_cookie_file`, `set_task_rate_limit`, `set_global_rate_limit`, `set_logger`), `Database` in `src-tauri/src/store/db.rs`, and `DownloadSpec` in `src-tauri/src/engine/worker.rs`. No Tauri command or event is added.
- Task filenames: `Engine::add_task` runs the platform-aware sanitizer for the current target before the name reaches the store or the in-memory spec. The sanitizer never returns an empty or dot-segment name, so task creation cannot persist a traversal or separator component.
- Part-file operations: part existence and removal go through `CapDir::exists` and `CapDir::remove_file`, which apply the same path validation, canonical-root check, and symbolic-link refusal as every other managed operation. A missing part is not an error on cancel, and a part that resolves outside the destination root is never touched.
- SQLite schema version 1: `schema_meta`, `tasks`, and `segments`. WAL, `synchronous=NORMAL`, foreign keys, a five-second busy timeout, and `0600` on the database plus `-wal` and `-shm` on Unix. Back up a non-empty database before a migration, migrate in one transaction, verify indexes and constraints, and refuse a newer schema. Timestamps are epoch milliseconds; offsets are signed 64-bit columns whose non-negative range preserves `u64`.
- Task states: `queued`, `connecting`, `downloading`, `paused`, `finalizing`, `completed`, `failed`, `cancelled`. Segment states: `pending`, `active`, `done`, `failed`. Only the checked transitions persist; anything else returns `INVALID_STATE`. `completed` requires a final file and matching stored hash; `paused` and `failed` preserve the part; `cancelled` removes only its own part.
- Queue: FIFO claim of the oldest queued task. The concurrency limit counts only `connecting`, `downloading`, and `finalizing`; a paused task does not consume a slot; a limit change takes effect without a restart.
- Cross-engine coordination: more than one `Engine` may open the same database. A pause or cancel applied through one instance is reconciled from the reloaded stored row by the other, so a concurrent pause ends `paused` and a concurrent cancel ends `cancelled` instead of `failed` or `INVALID_STATE`. A cancel that wins during finalization removes the part and leaves no final file, and a finalizing replacement that loses to a cancel removes the file it created.
- Data truth: the part file is `<sanitized filename>.oneget.part` beside the destination and is the byte truth; SQLite segment rows are the progress truth. A known total is preallocated sparsely through `PartFile::preallocate` and `CapDir::preallocate_sparse`, an unknown total starts empty, and writes use absolute `u64` offsets. A preallocation failure on a full device maps to `DISK_FULL` and pauses the task; other preallocation failures fail it. Progress persists at least once per 128 KiB read chunk.
- Finalization: stop workers, sync the part, stream SHA-256, persist the computed hash before the rename, verify an expected hash case-insensitively, choose a unique `stem (n).ext` name on collision, replace through `CapDir` without overwriting or following links, and sync the parent directory. A checksum mismatch fails with `CHECKSUM_MISMATCH` and creates no final file. A cancel from another engine instance that lands during finalization stays authoritative: the part is removed and no final file is left behind.
- Recovery: interrupted `connecting` or `downloading` tasks requeue when the part exists and fail with `PART_MISSING` otherwise; `finalizing` tasks re-finalize idempotently; a final-only file must match its stored hash, or, without one, the recorded total plus any expected hash; a cancelled leftover part is removed; paused and failed parts stay.
- Probe and Range: one ranged `GET` with `Range: bytes=0-0` and `Accept-Encoding: identity`; classify 206, 200, 416, HTML, and HTTP errors without reading the body. HTTP/1.1 only, no automatic redirects, ten manual hops, 15-second connect and 30-second read timeouts, no compression, and no public-network dependency. Revalidate the first range before the pool; a 200 with no bytes written falls back to one connection, and a 200 after bytes exist fails with `RANGE_IGNORED` without writing.
- Planning and splitting: 1 to 64 connections, half-open gap-free segments, 1 MiB minimum except the whole file or final remainder, and one open-ended segment for unknown length or no Range. A segment may split at most twice, only with an idle worker, at least two minimum segments remaining, and a split point ahead of the current offset; persist the shortened range and its tail in one transaction.
- Retry and faults: at most five attempts with exponential backoff and full jitter between 50 milliseconds and two seconds. Integer-seconds `Retry-After` is honored exactly; an HTTP-date or malformed value uses the exponential fallback. Do not retry 401, 403, 404, HTML, checksum, or a Range-ignored failure after data. A short body resumes at the persisted offset, an overlong body is truncated at the segment end, an invalid Content-Range start is `RANGE_MISMATCH` without writing, and ENOSPC pauses with `DISK_FULL`.
- Rate limiting: a byte leaky bucket paces each streamed chunk. Per-task and optional global limits share the schedule across a task's workers; zero or absent is unlimited.
- Network boundaries: an HTTP proxy URL must not embed userinfo, and optional basic authentication stays in memory. Netscape cookies match host, include-subdomains, path, secure, and expiry; they are reloaded at task start or resume and recomputed per redirect hop. Reject `Host`, `Authorization`, `Cookie`, `Range`, `Proxy-Authorization`, `Content-Length`, `Connection`, `Transfer-Encoding`, `Upgrade`, `TE`, `Trailer`, and `Set-Cookie` custom headers, and reject CR or LF in names or values.
- Privacy: header values, cookie values, proxy credentials, authorization values, and URL query or fragment tokens are never persisted or logged. Safe header names may be stored with empty values; values stay in memory only. The stored task URL strips userinfo, query, and fragment. Logs carry event names, task and segment identifiers, byte counts, and stable error codes only. No telemetry, remote reporting, SOCKS5, system-proxy discovery, browser-cookie extraction, or HuggingFace request is added.
- Platform: macOS uses sparse `set_len`; Windows attempts `FSCTL_SET_SPARSE` before extending and reports sparse or an explicit fallback. Both use the capability-scoped replace path.
- Harness and soak: `cargo run --bin engine_harness -- run|status|pause|resume|cancel` against a local fixture. The short smoke runs in `cargo test --test soak`; the ignored long run is `ONEGET_SOAK_SECONDS=86400 cargo test --test soak -- --ignored --nocapture` in `src-tauri/` or `bash scripts/soak-test.sh 86400`.

## Command and event contract

- Commands: `app_info`, `settings_get`, `settings_save`, `deep_link_last`, `diagnostics_secret_roundtrip`, `diagnostics_notification_status`, `diagnostics_send_notification`, `diagnostics_autostart_get`, `diagnostics_autostart_set`, and `app_graceful_quit`.
- Event: `app://deep-link` with `{ accepted, url?, error? }`.
- Rust payloads use camelCase and typed `{ code, message }` errors.
- The frontend calls only `src/lib/tauri.ts`. A missing Tauri runtime yields `runtime_unavailable`; the shell must not crash in a browser preview.
- Blocking keyring, file, and settings work runs through `spawn_blocking`; no command blocks the UI thread.
- The download engine is a Rust library and harness surface; it adds no command or event. A future UI must call the engine rather than duplicate its network or store logic.

## Security invariants

- No analytics, telemetry, remote script sources, or remote connect sources. The production CSP is bundled-only except IPC; keep the no-telemetry rule when adding any network behavior.
- Deep links accept only `oneget://download` with exactly one `url` parameter, an `http` or `https` target, no embedded credentials, valid percent encoding, and at most 8 KiB.
- Credential values live only in the operating-system credential store. Never write them to settings, logs, diagnostics payloads, or frontend state; diagnostics return booleans only. Values are redacted in formatting and zeroized on drop.
- Managed file access goes through `CapDir` under a canonical root. Reject absolute paths, parent or current components, NUL bytes, and symbolic-link or junction escapes; atomic replace refuses link sources and destinations.
- Sanitize user-supplied filenames with the platform-aware sanitizer before use.
- Manifest verification uses canonical JSON with sorted keys and no whitespace, rejects duplicate keys and input above 1 MiB, removes the top-level signature before hashing, and uses `verify_strict`. The verifier does not install, download, or execute components.
- Child processes never use a shell; they run in a Unix process group or Windows Job Object with whole-tree kill and timeout.
- The main window hides on close; the tray provides Show and Quit; graceful quit records a shutdown event and exits with code 0.
- The engine store is the progress authority and the part file is the byte authority. Never write a full `200` body at a nonzero offset, never produce a partially written final file, and never mark a task completed without a matching stored hash.
- The engine rejects non-HTTP schemes, credentialed targets, HTTPS-to-HTTP redirects, URLs over 8 KiB, forbidden headers, and CRLF-bearing header values before a request.
- Cookie values, proxy credentials, authorization values, URL query tokens, and custom header values never enter the SQLite store or the structured log. The store keeps only necessary recovery fields.

## Settings, logging, and errors

- Settings use `schemaVersion: 1`, default `theme: system` and `logLevel: info`, preserve unknown top-level fields, save atomically through a same-directory temporary file and rename, back up corrupt documents to a `.corrupt` sibling, and treat a future schema as read-only.
- Logs are JSON lines with `timestamp`, `level`, `event`, and redacted `fields`. Redact credential-bearing URL parts, authorization and cookie headers, URL userinfo, proxy credential fields, and provider credential prefixes before serialization.
- Rotation defaults to 10 MiB per file and five files including the current file; a record is never split across files. Debug builds also write the redacted line to stderr.
- Errors are user-safe typed `AppError` values with stable codes and no internal paths, stack traces, or credential values. `EngineError` follows the same rule and carries an internal retryability flag and optional retry delay.
- The task database is not a settings store. It holds minimized task URLs, filenames, states, offsets, remote metadata, hashes, error codes, connection counts, and safe header names with empty values.

## Quality gates

Run from the repository root unless noted:

- `npm ci`
- `npm run check:pins`
- `npm run typecheck`
- `npm run lint`
- `npm run test`
- `npm run build`
- `npx playwright install chromium` once, then `npm run test:e2e`
- `cargo check --locked`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test --locked` in `src-tauri/`
- `cargo test --test soak` in `src-tauri/` for the short smoke; `bash scripts/soak-test.sh 86400` for the ignored long soak and its duration record
- `npm run tauri build -- --bundles app` on macOS or `npm run tauri build -- --bundles nsis` on Windows
- `bash -n scripts/*.sh` before using the macOS signing helpers

## CI matrix

- Frontend job on Ubuntu: locked install, pin check, typecheck, lint, unit tests, build, and Playwright.
- Rust gates on `macos-15` and `windows-2025`: format, clippy with warnings denied, and tests.
- Build matrix: `macos-15` for `aarch64-apple-darwin` (`.app`), `macos-15-intel` for `x86_64-apple-darwin` (`.app`), and `windows-2025` for `x86_64-pc-windows-msvc` (NSIS), with uploaded artifacts.
- A macOS signing dry-run job creates an ephemeral certificate, imports it into a temporary keychain, signs and verifies a probe, cleans up, and checks fail-closed missing inputs.
- The workflow uses read-only permissions and concurrency cancellation. Hosted CI is green only with a real run identifier: the workflow's first remote run, `ci #1` (run 43143112787), passed all seven jobs on the implementation branch and uploaded the `oneget-aarch64-apple-darwin`, `oneget-x86_64-apple-darwin`, and `oneget-x86_64-pc-windows-msvc` artifacts. This records hosted CI only; manual platform acceptance, Windows install, and the Intel launch remain unproven.
