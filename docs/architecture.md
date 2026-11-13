# OneGet Architecture

This document describes the delivered OneGet desktop downloader: its task-first React interface, Tauri command surface, long-lived engine service, headless download engine, durable task store, and platform layer. It covers the current layers, module boundaries, trust boundaries, platform differences, data semantics, and verification baseline. Browser extension, video, HuggingFace, updater, release, and telemetry capabilities are not part of this delivery.

## Goals and target platforms

The delivered product provides a single-window desktop downloader and a headless download engine that:

- launches as one application instance and opens task-first with stored downloads, filters, search, and a collapsible details panel;
- adds one download or a batch, probes remote metadata, and accepts or rejects input before a task is created;
- controls tasks individually or in bulk, shows per-segment progress, and opens or deletes results safely;
- persists general and download settings, including language, and applies them without restart where documented;
- accepts a narrow `oneget://` deep-link contract and reports accepted or rejected results to the UI;
- integrates with a tray menu, login autostart, operating-system notifications, and the platform credential store;
- confines file access to capability-scoped directories and sanitizes user-supplied filenames;
- verifies signed component manifests without installing or executing components;
- probes remote files without consuming the body, plans bounded segments, and downloads through a bounded worker pool;
- persists tasks and segment progress in SQLite so pause, resume, cancel, and crash recovery survive a process restart;
- finalizes downloads atomically with a streamed SHA-256 and a unique destination name;
- enforces byte-rate limits, an HTTP proxy, scoped cookies, and safe custom headers without persisting secrets;
- writes structured, redacted, rotating logs and durable typed settings;
- builds with exact frontend and Rust dependencies and runs the same quality gates in CI;
- ships no remote script sources, no analytics, and no telemetry.

| Target | Architecture | Minimum or supported version |
| --- | --- | --- |
| macOS | arm64 and x86_64 | macOS 13 or later |
| Windows | x64 | Windows 10 22H2 or Windows 11 |

The React UI is the desktop surface, and it reaches the engine only through the typed Tauri command surface and the long-lived engine service. `localdocs/` is local reference material excluded from the bundle; it is not shipped product documentation.

## Repository layout

| Path | Responsibility |
| --- | --- |
| `src/` | React 19 and TypeScript 6 task-first user interface, downloads and settings features, i18n, design-system primitives, diagnostics, typed IPC wrappers, and deep-link event subscription. |
| `src/features/downloads/` | Task list and virtualization, categories, sorting, filtering, search, polling, new and batch dialogs with the native directory picker, probe handling, controls, details, deletion confirmations, localized engine-error mapping, and formatting. |
| `src/features/settings/` | General and download settings, the read-only network placeholder, and live language, theme, close-behavior, and engine-limit application. |
| `src/i18n/` | Simplified Chinese and English dictionaries, system-language resolution, and locale-aware formatting. |
| `src-tauri/src/` | Rust crate: Tauri runtime wiring, command layer, long-lived engine service, platform layer, download engine, task store, manifest verifier, settings, logging, and typed errors. |
| `src-tauri/src/commands/` | Thin Tauri command wrappers plus directly testable download, settings, diagnostics, and quit service functions. |
| `src-tauri/src/engine/` | Probe, safe HTTP transport, planner, part file, conflict-aware part naming, worker pool, scheduler, engine service actor, retry, rate limiting, cookies, destinations, final-URL recording, deletion, and state machines. |
| `src-tauri/src/store/` | SQLite connection setup, schema version 1 and 2 migrations, per-task destinations, part names, minimized final URLs, task and segment records, and progress persistence. |
| `src-tauri/src/bin/engine_harness.rs` | Command-line harness to add, run, inspect, pause, resume, and cancel one task without printing secrets. |
| `src-tauri/tests/` | Rust integration suites for download commands, paths, filenames, files, the opener, processes, secrets, logging, settings, manifests, the shell contract, the store, the engine, and the soak. |
| `src-tauri/tests/support/` | Deterministic local range, proxy, cookie, fault, and fixture helpers on ephemeral ports. |
| `scripts/soak-test.sh` | macOS wrapper for the ignored long-running HTTP-only soak with a caller-supplied duration. |
| `src-tauri/capabilities/` | Minimal Tauri capability for the main window. |
| `src-tauri/tauri.conf.json` | Product identity, window, CSP, deep-link scheme, and bundle configuration. |
| `scripts/` | Dependency-pin check and macOS local signing helpers. |
| `e2e/` | Playwright browser checks for the shell, design system, downloads, settings, and keyboard flows, backed by deterministic mock IPC. |
| `.github/workflows/ci.yml` | Frontend, Rust, build-matrix, and macOS signing dry-run jobs. |
| `assets/` and `src-tauri/icons/` | Source and generated application icons. |
| `extension/` | Extension placeholder; extension behavior is not implemented. |
| `localdocs/` | Ignored local reference material, not shipped documentation. |

## Layer and module boundaries

### React UI

`src/main.tsx` mounts `src/App.tsx`, which wraps `src/app/AppShell.tsx` in `src/app/ErrorBoundary.tsx` and the i18n provider. The shell is task-first: a sidebar carries All, Downloading, Completed, Failed, and type categories with counts; a toolbar carries New, Batch, Pause All, Resume All, search, and Settings; the center is the task list; a collapsible details panel shows the selected task; and a status bar reports active tasks and total speed. The empty state explains how to add a download. `src/components/ui/` owns the Button, Input, Dialog, Select, ProgressBar, Badge, Checkbox, and Toast primitives. The shell mounts `Toast` for localized success feedback. `src/features/diagnostics/DiagnosticsPanel.tsx` keeps the app-information, deep-link, autostart, notification, secret round-trip, and graceful-quit diagnostics, but the task-first shell does not mount it; the panel remains a retained module exercised by its own tests.

`src/features/downloads/` owns the download surface. `TaskList` renders a fixed-height `VirtualList` with overscan, so only a bounded window of rows is mounted even for 10,000 tasks. Categories derive from the sanitized final filename extension and are matched case-insensitively; sorting covers creation time, filename, size, progress, speed, and a fixed state order with creation time as the tiebreaker; filters and keyword search operate on the full data set. `useDownloads` polls `downloads_list` at most once per second while the document is visible, pauses polling while hidden, and refreshes after control actions. `NewDownloadDialog` and `BatchDownloadDialog` use the localized `@tauri-apps/plugin-dialog` 2.8.4 directory picker, validate URLs, filenames, destinations, connections, rate limits, checksums, and safe headers, and render probe states including HTML allowance and ignored Range. Connection input accepts 1 to 64 within the `u32` range, rate input must be a positive whole number at most `Number.MAX_SAFE_INTEGER`, the largest value the IPC number boundary carries without rounding, and an invalid destination surfaces as the destination-specific `INVALID_DESTINATION` action error. `TaskRow`, `TaskDetails`, and the deletion confirmation components cover state-aware controls, redacted stored and final URLs, segments with half-open ranges, open and copy actions, and both deletion paths.

`src/features/downloads/errorMessages.ts` maps each known stable engine code to a dictionary key, so the interface renders localized guidance instead of raw engine prose; `CHECKSUM_MISMATCH` tells the user to retry only with the correct expected SHA-256 or without it. Success actions show localized toasts. `TaskDetails` treats a segment end as exclusive, so a one-byte segment renders as complete rather than off by one.

`src/features/settings/` owns the general and download settings plus a read-only network placeholder. Language and theme apply immediately, defaults apply to new downloads, and concurrency and the global rate limit apply to new tasks after saving while running downloads keep their captured limits. `src/i18n/` resolves the system language, switches between Simplified Chinese and English without restart, and formats numbers, sizes, speeds, ETA, and dates per locale.

`src/lib/tauri.ts` is the only generic command entry point, and `src/lib/downloads.ts` wraps the download commands on top of it. A missing Tauri runtime returns `runtime_unavailable` instead of throwing. `src/lib/events.ts` subscribes to the `app://deep-link` event and becomes inert without a runtime. `src/types/app.ts` and `src/types/downloads.ts` freeze the command arguments and result shapes.

### Tauri runtime and command layer

`src-tauri/src/main.rs` delegates to `oneget_lib::run`. `src-tauri/src/lib.rs` registers the single-instance, deep-link, autostart, and notification plugins in that order, builds the tray, registers schemes, wires the deep-link event, opens the task database, starts the engine service with recovery, and initializes state. Closing the main window follows the configured close behavior: hide to the tray or quit after a graceful engine shutdown.

`src-tauri/src/app_state.rs` owns resolved paths, the settings store, the logger, service implementations, the engine service handle, and the last deep-link result. `src-tauri/src/commands/mod.rs` contains the thin Tauri command wrappers and moves blocking work through `spawn_blocking`. `src-tauri/src/commands/app.rs` and `downloads.rs` contain directly testable service functions, and `src-tauri/src/engine/service.rs` owns the long-lived engine actor.

The service starts one dedicated thread with a current-thread Tokio runtime because the engine and SQLite connection are not shared across threads. Every caller request is a channel command, and the engine run loop is polled concurrently with those requests, so pause, resume, cancel, retry, probe, and deletion work while downloads progress. A slow probe runs alongside the request and run loops instead of blocking them, so a pending probe does not stall task listings or controls. Startup recovery completes before the service is returned, and paused-task revalidation is guarded to run once per engine instance rather than on every run-loop restart. `shutdown` pauses active downloads, stops the request loop, and refuses later work.

The command and event contract is:

| Surface | Kind | Purpose |
| --- | --- | --- |
| `app_info` | command | Product name, version, and current log path. |
| `settings_get` | command | Current typed settings. |
| `settings_status` | command | Read-only and corrupt-recovery state of the settings document. |
| `settings_save` | command | Persist settings, apply live engine limits, and return the stored value. |
| `downloads_list` | command | Lightweight summaries of every stored task, including the minimized post-redirect `finalUrl`. |
| `downloads_segments` | command | Segment rows for one task's details panel. |
| `downloads_probe` | command | Remote metadata for a URL before task creation. |
| `downloads_add` | command | Create one queued task and return its identifier. |
| `downloads_pause` | command | Pause an eligible task. |
| `downloads_resume` | command | Re-probe and resume an eligible task. |
| `downloads_cancel` | command | Cancel a task and remove only its own part. |
| `downloads_retry` | command | Requeue a failed task while preserving its part. |
| `downloads_delete_record` | command | Delete a terminal record, removing only its own part. |
| `downloads_delete_file` | command | Delete a completed final file and its record. |
| `downloads_open_file` | command | Open a completed file with the platform handler. |
| `downloads_open_folder` | command | Open a task's recorded destination folder. |
| `deep_link_last` | command | Most recently handled deep-link result. |
| `diagnostics_secret_roundtrip` | command | Store, read, and delete a generated diagnostic value; returns a boolean. |
| `diagnostics_notification_status` | command | Current notification permission. |
| `diagnostics_send_notification` | command | Send a diagnostic notification. |
| `diagnostics_autostart_get` | command | Current autostart registration state. |
| `diagnostics_autostart_set` | command | Enable or disable autostart and return the resulting state. |
| `app_graceful_quit` | command | Write a shutdown record and exit with code 0. |
| `app://deep-link` | event | `{ accepted, url?, error? }` for accepted or rejected links. |

Rust payloads serialize camelCase. Errors use a typed `{ code, message }` shape. The UI uses bounded polling rather than a progress event stream: `downloads_list` runs at most once per second while the window is visible, stops while it is hidden, and refreshes immediately after a control action or a completed mutation. A missing engine reports `engine_unavailable`, and a missing Tauri runtime reports `runtime_unavailable`.

### Platform layer

`src-tauri/src/platform/` is the only place where target-specific branches live. It owns:

- application paths for data, cache, logs, downloads, and backups;
- deterministic filename sanitization for macOS and Windows rules;
- `CapDir`, the canonical capability root for asynchronous create, read-write open, absolute-offset write, sparse preallocation, read, metadata, file and parent-directory sync, and atomic replace;
- `ChildGuard`, which owns a Unix process group or Windows Job Object and supports whole-tree termination and timeouts;
- the keyring-backed secret store and the in-memory fake;
- the shell-free opener that launches a file or folder through `open`, `explorer.exe`, or `xdg-open` without shell interpretation, detached and fire-and-forget with no process group or Job Object;
- deep-link parsing and runtime scheme registration;
- the tray, autostart, and notification plugin adapters.

Shared modules call these interfaces and do not branch on the target operating system. The platform layer exposes typed `PlatformError` values with stable codes.

### Download engine

`src-tauri/src/engine/` owns the headless download path. `probe.rs` classifies remote metadata without reading the body. `http.rs` validates URLs and custom headers, follows redirects manually, strips sensitive headers across origins, and builds the HTTP/1.1 client. `planner.rs` produces bounded half-open ranges. `part_file.rs` owns the sparse preallocated part file, absolute-offset writes, and streamed hashing, routing preallocation through `CapDir`. `worker.rs` runs the bounded pool, dynamic splitting, Range fallback, retry, atomic finalization, and final-URL recording. `scheduler.rs` owns the FIFO queue, concurrency limit, pause, resume, cancel, retry, conflict-aware part naming, startup recovery, and reconciliation of controls applied through another engine instance. `retry.rs`, `rate_limit.rs`, and `cookies.rs` own the retry policy, byte pacing, and cookie scope. `error.rs` and `state.rs` own stable codes and checked transitions.

`Engine::add_task` sanitizes the caller-supplied filename with the platform naming rules for the current target before it reaches the store or the in-memory spec, and part-file existence and removal go through the validated `CapDir::exists` and `CapDir::remove_file` operations. A task normally uses `<sanitized filename>.oneget.part`; only when a same-filename task conflicts in the effective destination does it use `<sanitized filename>.<first 8 of task id>.oneget.part`. Conflict detection compares the effective destination, so a version-1 row without a stored destination falls back to the engine default, and it includes any same-filename task that is not completed or cancelled plus terminal rows whose part file still exists. The conflict check, part-name choice, and task insert run in one `BEGIN IMMEDIATE` transaction, so two engine instances cannot both claim the same name. The chosen name is persisted in `part_path` and is the single part identity for download, resume, finalize, cancel, recovery, deletion, and existence checks, with the legacy name as fallback for rows written without it.

The engine exposes a library API: `Engine::new`, `add_task`, `run`, `pause`, `resume`, `cancel`, `retry`, `recover`, `delete_record`, `delete_file`, `open_file`, `open_folder`, `set_max_concurrent`, `set_proxy`, `set_cookie_file`, `set_task_rate_limit`, `set_global_rate_limit`, and `set_logger`. The desktop application reaches it through `EngineService` in `src-tauri/src/engine/service.rs`, which exposes `start`, `list`, `segments`, `probe`, `add`, `pause`, `resume`, `cancel`, `retry`, `delete_record`, `delete_file`, `open_file`, `open_folder`, `apply_settings`, and `shutdown`. `src-tauri/src/bin/engine_harness.rs` remains the command-line surface for manual checks; it supports `run`, `status`, `pause`, `resume`, and `cancel`, and prints no URL, cookie, proxy credential, or header value.

Each task's destination is resolved and validated when the task is created, persisted with the task, and reused for its part and final file after a restart. A stored task without a destination falls back to the engine's configured default directory, which is the settings default directory or the platform downloads directory. A missing, non-directory, unreadable, or unwritable destination produces a typed `INVALID_DESTINATION` error instead of creating a task. The post-redirect target is persisted in the same minimized form as the task URL, with userinfo, query, and fragment removed, and is exposed as `finalUrl` in list summaries and details.

Deletion has two distinct paths. Deleting a record is allowed only for completed, failed, or cancelled tasks; it removes the task's recorded part when present and keeps any completed final file. Deleting a file is allowed only for a completed task; a second confirmation in the UI precedes it, and the engine validates the final file through the capability root before removing that file and then the record. A missing final file returns `FILE_MISSING` and keeps the record. Opening a file also requires a completed task and an existing final file, and opening a folder uses the recorded destination; both go through the shell-free platform opener.

### Durable task store

`src-tauri/src/store/` owns SQLite. `Database::open` configures WAL, `synchronous=NORMAL`, foreign keys, and a five-second busy timeout; on Unix it restricts the database and its `-wal` and `-shm` sidecars to mode `0600`. `migrations.rs` creates schema version 1 and migrates it to version 2 by adding the per-task destination, completion timestamp, and final URL, backs up an existing non-empty database before a migration, applies each migration in one transaction, verifies the required indexes and constraints, and refuses a newer schema with a typed error. Existing version-1 rows keep a null destination and null final URL; the destination falls back to the configured default on recovery. `tasks.rs` and `segments.rs` own task and segment persistence; `types.rs` exposes the public records.

### Kernel manifest verifier

`src-tauri/src/kernel/manifest.rs` canonicalizes JSON by sorting object keys recursively and emitting no insignificant whitespace. It rejects duplicate keys, non-object roots, malformed input, and payloads above 1 MiB. Verification removes the top-level `signature` field, decodes the Ed25519 signature and public key, and calls `verify_strict`. The module is a verification primitive only: it contains no installer, downloader, signing key material, or component execution.

### Configuration, logging, and errors

`src-tauri/src/config.rs` owns schema version 2 settings: theme, log level, language, close behavior, optional default directory, default connections, maximum concurrent tasks, and an optional global rate limit, with system-oriented defaults. Loading migrates a version-1 document in memory while preserving theme, log level, and unknown top-level fields, creates defaults when absent, backs up corrupt documents to a `.corrupt` sibling, and treats a document from a newer schema as read-only. Saves use a same-directory temporary file, sync, and rename.

`src-tauri/src/logging/` owns JSON-line logging, redaction, and size rotation. `src-tauri/src/error.rs` owns the typed application error. Details are in the baseline section below.

### Tests and quality tooling

Frontend behavior tests live beside components and in `src/features/`, including the i18n audit, localization of every known engine error code, validation bounds, half-open segment rendering, and virtualization tests. Browser checks live in `e2e/` and use deterministic mock IPC from `e2e/support/mock-ipc.ts`, which stubs the registered list, probe, control, deletion, open, and settings commands and their typed results. The mock layer exercises new download, pause and resume, cancel, record and file deletion, settings persistence, language switching, keyboard flows, and 10,000-row scrolling without a public network or real credential. Rust integration suites live in `src-tauri/tests/`, including `downloads_commands`, `config`, `store`, and `platform_opener` alongside the retained engine suites; they cover minimized final-URL persistence through redirects, same-name task part isolation, and the destination picker's typed error path. `scripts/check-direct-dependency-pins.mjs` rejects floating direct npm versions. `scripts/soak-test.sh` runs the ignored long soak for a caller-supplied duration. The macOS signing helpers are local verification tools, not release signing.

## Download engine and task store

### SQLite schema version 2

The store opens with WAL journaling, `synchronous=NORMAL`, foreign-key enforcement, and a five-second busy timeout. A non-empty database is backed up beside itself before a migration. Version 1 creates `tasks`, `segments`, and `schema_meta`; version 2 adds `destination`, `completed_at_ms`, and `final_url` to `tasks` and records the new version. Each migration runs in one immediate transaction that re-reads the current schema version before applying work, verifies the required indexes and constraints, and refuses a schema newer than version 2 with a typed error. On Unix the database and its `-wal` and `-shm` sidecars are restricted to mode `0600`.

| Table | Columns and constraints |
| --- | --- |
| `schema_meta` | `key` primary key and `value`; stores `schema_version`. |
| `tasks` | `id` primary key, `url`, nullable `final_url`, `filename`, nullable `destination`, nullable `part_path`, checked `state`, non-negative `downloaded_bytes`, nullable non-negative `total_bytes`, `expected_sha256`, `actual_sha256`, `etag`, `last_modified`, `error_code`, `error_message`, epoch-millisecond `created_at_ms`, `updated_at_ms`, and nullable `completed_at_ms`, positive `connections`, and `headers_json`. |
| `segments` | Composite primary key `(task_id, segment_index)`, `start_offset` and `end_offset` with `end_offset > start_offset`, non-negative `downloaded_bytes` bounded by the range length, checked `state`, and a cascading foreign key to `tasks`. |

Indexes cover `(state, created_at_ms)` for queue claims and `task_id` for segment lookup. Offsets use signed 64-bit columns whose non-negative range preserves every `u64` file position. Segment ranges are half-open: `end_offset` is exclusive. The stored URL and post-redirect `final_url` are a minimized form: userinfo, query, and fragment are removed before persistence, and the full URL stays in memory only. `headers_json` stores safe header names with empty values, never header values.

### Task and segment state machines

Task states are `queued`, `connecting`, `downloading`, `paused`, `finalizing`, `completed`, `failed`, and `cancelled`. The checked transitions are:

- `queued` to `connecting`, `paused`, or `cancelled`;
- `connecting` to `queued`, `downloading`, `paused`, `failed`, or `cancelled`;
- `downloading` to `queued`, `paused`, `finalizing`, `failed`, or `cancelled`;
- `paused` to `queued`, `failed`, or `cancelled`;
- `finalizing` to `completed`, `failed`, or `cancelled`;
- `failed` to `queued` or `cancelled`;
- `cancelled` to `cancelled` only.

Any other transition returns `INVALID_STATE` and leaves the row unchanged. `completed` requires a final file and a matching stored hash. `paused` preserves the part and segment progress. `cancelled` removes only the task's own part. `failed` preserves the part for retry. Segment states are `pending`, `active`, `done`, and `failed`, with `pending` to `active` or `failed`, `active` to `done`, `failed`, or `pending`, and `failed` to `pending`.

### Queue, concurrency, and lifecycle controls

The scheduler claims the oldest queued task and never exceeds the configured limit across `connecting`, `downloading`, and `finalizing` tasks. A paused task does not consume a slot; a finalizing task does. Completion, failure, cancellation, a limit change, and new work wake the scheduler through a notification. The limit can change without restarting the engine. Pause persists progress and stops workers promptly. Resume re-probes the remote file and rejects a changed ETag, Last-Modified, or total size with `FILE_CHANGED`; an unchanged task requeues and continues from persisted offsets. A missing part fails with `PART_MISSING`. Cancel is idempotent where defined, removes only the task's part, and never creates a final file.

More than one `Engine` may open the same database. When one instance pauses or cancels a task that another instance is running, the running instance reloads the stored row on an `INVALID_STATE` transition or a control point and adopts the stored outcome: a concurrent pause finishes as `paused` and a concurrent cancel as `cancelled`, never `failed` or `INVALID_STATE`. Pause persistence does not overwrite a cancellation, a cancel during finalization removes the part and leaves no final file, and a finalizing replacement that loses the completion race removes the file it created.

### Probe, redirects, and Range fallback

A probe sends one ranged `GET` with `Range: bytes=0-0` and `Accept-Encoding: identity`, and classifies headers without reading the body. A valid `206` reports range support and the exact total; a `200` reports no range support with any Content-Length total; a `416` with a zero total yields an empty plan; a nonzero `416` safely retries without Range. HTML returns `HTML_RESPONSE` unless explicitly allowed. `401` and `403` map to `AUTH_REQUIRED`, `404` to `NOT_FOUND`, `429` to `RATE_LIMITED`, and `5xx` to `SERVER_ERROR`.

The client is HTTP/1.1 only, never auto-follows redirects, and never negotiates compression. Redirects are followed manually up to ten hops; non-HTTP schemes, HTTPS-to-HTTP downgrade, credentialed targets, control characters, and URLs over 8 KiB are rejected. Cookies and sensitive headers are recomputed or stripped when a hop crosses origins, and the final URL is recorded.

A probe result is advisory. After planning, the engine revalidates the first range with a small prefix request before starting the full pool. A `200` at that point with no bytes written discards the multi-connection plan and restarts as one connection from zero. A later `200` to a ranged request after bytes exist fails with `RANGE_IGNORED`, preserves the part, and creates no final file. A full `200` body is never written at a nonzero offset.

### Segment planning, splitting, and part files

The planner accepts 1 to 64 connections and creates `min(connections, max(1, total / 1 MiB))` non-overlapping, gap-free half-open segments that cover the whole known file. A zero total produces an empty ranged plan. An unknown length or a server without range support produces one open-ended segment. Segments smaller than 1 MiB appear only when the whole file is smaller or as a final remainder.

Each task owns one part file beside its destination. The filename is sanitized for the current target when the task is created. A task normally uses `<sanitized filename>.oneget.part`; only when a same-filename task conflicts in the effective destination does the engine choose `<sanitized filename>.<first 8 of task id>.oneget.part`, so same-name tasks never share a part. The effective destination is the stored one, or the engine default for a version-1 row without a destination; any same-filename task that is not completed or cancelled conflicts, as does a terminal row whose part file still exists. Conflict detection, part-name choice, and the task insert share one `BEGIN IMMEDIATE` transaction, so concurrent engines cannot both claim a name. The chosen name is persisted in `part_path` and reused with the legacy-name fallback by download, resume, finalize, cancel, recovery, deletion, and existence checks. A known total is preallocated sparsely through `PartFile::preallocate`, which calls `CapDir::preallocate_sparse`; a full device maps to `DISK_FULL` and pauses the task, and an unknown total starts empty. Workers write at absolute `u64` offsets. Part existence and removal use the validated `CapDir::exists` and `CapDir::remove_file` operations, so a part that resolves outside the destination root is never inspected or removed. Progress is persisted in the database in short transactions, with segment rows as the progress truth and the part file as the byte truth. Each task's worker pool is bounded by its connection count.

When an idle worker exists, the scheduler may split the active segment with the most remaining bytes, provided at least two minimum segments remain and the split depth is below two. A split point is set ahead of the current offset, the shortened segment and its tail are persisted in one transaction, and workers check the split point before every write. A split the worker has already passed is abandoned. The invariant stays non-overlapping, gap-free coverage of the whole file.

### Finalization, hashing, and recovery

Finalization stops workers, syncs the part file, streams SHA-256 over the bytes, and persists the computed hash before the rename. A supplied expected hash is compared case-insensitively. A unique final name is chosen when the destination exists; the capability-scoped replace refuses symbolic-link sources and destinations and never overwrites an existing file. The parent directory is synced after the rename, and only then does the task become `completed`. A checksum mismatch fails with `CHECKSUM_MISMATCH`, preserves the part, and creates no final file. A cancellation from another engine instance during finalization stays authoritative: the part is removed and no final file is left, and a completion that loses the state race removes the file it just renamed. A completed task records its completion time, which the details panel displays.

Recovery is idempotent. Interrupted `connecting` or `downloading` tasks requeue when their recorded part exists, falling back to the legacy name for rows without a recorded path, and fail with `PART_MISSING` otherwise. A `finalizing` task with only the final file is accepted when its stored hash matches, or, without a stored hash, when its size equals the recorded total and a configured expected hash matches; the recomputed hash is stored. A final-only mismatch fails with `CHECKSUM_MISMATCH` and preserves the final file. A part-only task re-finalizes. A cancelled task has only its own recorded leftover part removed. Repeated finalization of a completed task returns the existing path without network access.

### Retries, timeouts, and disk-full handling

The client uses a 15-second connect timeout and a 30-second read timeout. Retryable transport, timeout, server, and rate-limit failures receive at most five attempts with exponential backoff and full jitter between 50 milliseconds and two seconds. An integer-seconds `Retry-After` is honored exactly; an HTTP-date or malformed value falls back to the exponential delay so a far-future date cannot stall a task. `401`, `403`, `404`, HTML, checksum failures, and a Range-ignored failure after data is written are not retried. A short body resumes from the persisted offset. An overlong body is truncated at the segment end. An invalid Content-Range start fails with `RANGE_MISMATCH` without writing. ENOSPC maps to `DISK_FULL` and pauses the task with the part preserved; resume continues when writes succeed again.

### Rate limiting, proxy, cookies, and headers

A byte-oriented leaky bucket paces each streamed chunk. Per-task and optional global limits share one schedule across a task's workers, so aggregate throughput stays at the configured bytes per second; zero or an absent limit is unlimited. The first chunk starts immediately.

The HTTP proxy URL must not contain userinfo; optional basic authentication stays in memory and is applied by the transport so proxy credentials never reach the origin, the store, or the log. Netscape cookie files are parsed for domain, include-subdomains, path, secure, and expiry. Only matching host and path cookies are sent; expired and secure-over-HTTP cookies are skipped; cross-host redirects recompute the header for the target host; the file is reloaded at each task start or resume.

Custom headers are validated before any request. The forbidden set is `Host`, `Authorization`, `Cookie`, `Range`, `Proxy-Authorization`, `Content-Length`, `Connection`, `Transfer-Encoding`, `Upgrade`, `TE`, `Trailer`, and `Set-Cookie`; CR or LF in a name or value is rejected. Header values are never persisted or logged. Safe header names may be stored with empty values so a fresh engine can report which headers were configured, but values are not replayable after the in-memory session.

### Platform differences

`PartFile::preallocate` delegates to the capability layer. On macOS and other Unix targets, `set_len` extends the part file and reports sparse preallocation. On Windows, `FSCTL_SET_SPARSE` is attempted before the length change; the result is reported as sparse or as an explicit fallback, and the file is still extended. The capability-scoped replace path is shared across targets and refuses link sources and destinations, and part existence and removal also stay behind `CapDir::exists` and `CapDir::remove_file`. The large-file suite uses a 5 GiB logical sparse part file and writes only head, middle, and tail ranges at absolute offsets; it does not download the full logical length.

### Fixtures, harness, and soak

Every network test uses a local ephemeral-port fixture. The range server covers normal ranged responses, ignore-range, a 206 probe with a later 200, an ignore-after-byte threshold, unknown length, HTML, truncation, redirects, authentication, rate limiting, server errors, 416, and a large sparse mode. Separate local fixtures cover proxy authentication and cookie files. The harness can run one task to completion and inspect, pause, resume, or cancel it using a shared database and destination. The short soak smoke runs in the normal test suite. The ignored soak repeats task creation, pause and resume, cancel, retry, and completion against the fixture, then verifies final hashes, `PRAGMA integrity_check`, orphan part files, and unhandled panics. Its summary contains counts and bytes only, never URLs, queries, cookies, or credentials.

## Trust boundaries

### WebView and content security policy

The WebView loads bundled assets only. The production CSP allows scripts, styles, fonts, images, and media from the application origin and permits IPC connections only through `ipc:` and `http://ipc.localhost`. The development CSP additionally allows the local Vite server. There is no remote script source, no remote connect source, no analytics dependency, and no telemetry endpoint.

### UI and IPC

The React UI is untrusted presentation. It reaches the backend only through typed wrappers over the registered commands, sends no filesystem path it has not validated through the same rules, and displays only redacted stored URLs and user-safe error messages. The engine revalidates URLs, filenames, destinations, headers, and state transitions at the command or engine boundary, so a compromised or faulty renderer cannot bypass capability checks, deletion preconditions, or secret redaction. The UI polls a bounded summary instead of receiving an event stream, which keeps the renderer from accumulating unbounded state.

### Deep links

Deep links are untrusted input. The parser accepts only a `oneget://download` link with exactly one `url` parameter. The target must parse as `http` or `https`, must not contain embedded credentials, must use valid percent encoding, must not contain ASCII control characters once percent-decoded, and the whole link is limited to 8 KiB. Missing, duplicate, malformed, non-http, credentialed, oversized, and control-character-bearing links produce typed rejections. Windows registers schemes at runtime; macOS relies on the built bundle registration.

### Credential storage

Credential values are held in the operating-system credential store through a `SecretStore` interface. Values are wrapped so formatting shows a redaction marker, and the wrapper zeroizes on drop. The diagnostic round-trip command stores, reads, compares, and deletes a generated value but returns only a boolean. Automated tests use an in-memory store and never touch the real keychain. Settings and logs never contain credential values.

### Capability-scoped files

Every managed write goes through `CapDir`. The root is created and canonicalized before use. Input paths must be relative, contain no NUL bytes or backslashes, and contain only normal components; absolute paths, parent components, and current components are rejected. Existing targets are canonicalized and rejected when they resolve outside the root. Writes and creates refuse a symbolic-link target, atomic replace refuses symbolic-link sources and destinations, and parent traversal is checked before creation. Asynchronous operations run on the blocking pool.

### Component manifests

A manifest signature is verified over canonical JSON with the top-level signature removed. Duplicate keys are rejected at every nesting level so the parsed and signed views cannot diverge. Signatures and keys must be exact hex lengths, and verification uses `verify_strict`. The verifier does not install, download, or execute anything.

### Child processes

No production code invokes a shell. A guarded child is spawned as a Unix process-group leader or inside a Windows Job Object with kill-on-drop. `kill_tree` terminates the whole tree and reaps the top-level child; `wait_timeout` kills the tree and returns a typed timeout. The drop guard signals a still-running tree. The shell-free platform opener is the deliberate exception: it spawns the operating-system handler detached and fire-and-forget, without a process group or Job Object, because the handler must outlive the opener request; guarding it would kill it.

### Settings and logs

Settings are not a secret store. Logs pass every event and field string through redaction before serialization, and object keys that denote credentials are replaced wholesale. Redaction covers credential-bearing URL parts, authorization and cookie headers, URL userinfo, proxy credential fields, and common provider credential prefixes. Errors are user-safe typed messages and do not carry internal paths, stack traces, or credential values.

## Platform differences

| Concern | macOS | Windows |
| --- | --- | --- |
| Data root | `~/Library/Application Support/com.oneget.app` | `%APPDATA%\OneGet` |
| Cache and log roots | `~/Library/Caches/com.oneget.app` and `~/Library/Logs/com.oneget.app` | `%LOCALAPPDATA%\OneGet\Cache` and `%LOCALAPPDATA%\OneGet\Logs` |
| Downloads | `~/Downloads` | `%USERPROFILE%\Downloads` |
| Credential service name | `com.oneget.app` | `OneGet` |
| Autostart | LaunchAgent through the autostart plugin | Run-key behavior through the autostart plugin |
| Deep-link registration | Bundle `oneget` scheme and LaunchServices | Bundle scheme plus runtime registration of every configured scheme |
| Notifications | Operating-system notification center; prompt behavior can depend on signature stability | Windows notification path requires the installed NSIS build, not the raw release binary |
| Sparse preallocation | `set_len` extends the part file and reports sparse | `FSCTL_SET_SPARSE` is attempted first; an unavailable sparse mode is reported as an explicit fallback |
| Atomic replace | Capability-scoped rename refuses link sources and destinations | The same capability-scoped replace path is used; no direct overwrite |
| Open file or folder | `open` without a shell; handler detached with no process guard | `explorer.exe` without a shell; handler detached with no process guard |
| Bundle format | `.app` | NSIS installer |

The non-macOS, non-Windows path code is a development fallback using XDG locations; it is not a shipped target.

## Logging, error, and configuration baseline

- Log records are JSON lines with an epoch-millisecond `timestamp`, lowercase `level`, redacted `event`, and recursively redacted `fields`.
- The default rotation is 10 MiB per file and five files including the current one. Records are never split across files. Debug builds also mirror the redacted line to stderr.
- The log level is part of settings and filters records above the configured verbosity.
- `AppError` serializes as `{ code, message }`, with stable codes such as `settings_read_only` and `settings_serialize`.
- Settings use `schemaVersion: 2` with `theme` (`system`, `light`, `dark`), `logLevel` (`error`, `warn`, `info`, `debug`, `trace`), `language` (`system`, `zh-CN`, `en-US`), `closeBehavior` (`tray`, `quit`), optional `defaultDirectory`, `defaultConnections` (8), `maxConcurrentTasks` (3), and optional `globalRateLimit` in bytes per second.
- A version-1 document migrates in memory without losing theme, log level, or unknown fields. Unknown settings fields survive a save. A future schema is never overwritten. A corrupt document is backed up before defaults are restored.
- The download engine adds one persistent product database at schema version 2. It is not a settings store and holds only task recovery fields: minimized URLs and final URLs, filenames, per-task destinations, part names, task and segment state, byte progress, remote metadata, hashes, error codes, connection counts, timestamps, and safe header names with empty values.
- Engine log events carry lifecycle names, task and segment identifiers, byte counts, and stable error codes. They never carry URLs with queries, cookie values, proxy credentials, authorization values, or custom header values.

## Test and CI baseline

Local macOS arm64 evidence covers the exact-pin check (27 entries), typecheck, lint, 33 Vitest files with 268 tests, the production build, and 30 Playwright tests across the chromium-light and chromium-dark projects against deterministic mock IPC. Rust evidence covers `cargo fmt -- --check`, `cargo check --locked`, `cargo clippy --all-targets -- -D warnings`, and 176 locked tests with 1 ignored long soak: config 10, downloads_commands 11, engine_download 10, engine_large 4, engine_network 23, engine_probe 12, engine_queue 21, engine_recovery 9, engine_redaction 2, engine_segments 6, logging_redaction 2, manifest 13, platform_filename 2, platform_files 7, platform_opener 2, platform_paths 3, platform_process 4, platform_secrets 6, shell_contract 13, short soak smoke 1, and store 15. The Tauri application bundle builds locally. One earlier full-suite run hit a transient short-soak failure under parallel infrastructure contention; the isolated soak and the following full run passed, so no product failure is claimed or inferred.

The CI workflow defines a frontend job, Rust quality gates on `macos-15` and `windows-2025`, a build matrix for `aarch64-apple-darwin`, `x86_64-apple-darwin`, and `x86_64-pc-windows-msvc`, and a macOS signing dry-run job that creates, imports, signs, verifies, and cleans up an ephemeral identity and fails closed on missing inputs. The hosted workflow's earlier shell-baseline run passed all seven jobs and uploaded the three build artifacts; this revision claims no new hosted run. Manual platform acceptance, the real-application local HTTP download, Windows install checks, and the macOS x86_64 Intel launch remain pending.

## Security invariants

1. The production WebView has no remote script or connect source, and the product carries no analytics or telemetry.
2. Credential values never appear in logs, settings, diagnostic payloads, or frontend state.
3. Managed filesystem access is confined to a canonical capability root; traversal, absolute paths, NUL bytes, and symbolic-link or junction escapes are rejected.
4. Deep links accept only one `oneget://download` link with one validated `http` or `https` target, no embedded credentials, valid encoding, and an 8 KiB limit.
5. Manifest verification uses canonical JSON, rejects duplicate keys, excludes the top-level signature, and uses strict Ed25519 verification; it never installs or executes a component.
6. Guarded child processes never use a shell and are terminated as a whole tree on timeout or drop; the shell-free opener launches the OS handler detached without a process guard by design.
7. Platform-specific code stays under `src-tauri/src/platform/`; shared modules do not branch on the target operating system.
8. Settings and logs are redacted or typed and never become a secondary secret store.
9. The main window hides on close, the tray provides Show and Quit, and graceful quit records a shutdown event before exiting with code 0.
10. The task store is the progress authority and the recorded part file is the byte authority. A completed task has a final file and a matching stored hash; a partially written final file is never produced.
11. Cookie values, proxy credentials, authorization values, URL query tokens, and custom header values never enter the store or the log. The stored URL and post-redirect final URL remove userinfo, query, and fragment.
12. The engine rejects non-HTTP schemes, credentialed targets, HTTPS-to-HTTP redirects, URLs over 8 KiB, forbidden headers, and CRLF-bearing header values before a request.
13. A ranged request that receives a full `200` after data exists fails with `RANGE_IGNORED` and never writes the body at a nonzero offset.
14. The engine makes no analytics, telemetry, remote reporting, system-proxy discovery, SOCKS5, browser-cookie-extraction, or HuggingFace request.
15. Record deletion is limited to terminal tasks and never removes a completed final file; file deletion is limited to completed tasks and removes only the capability-validated final file plus its record.
16. The platform opener launches a file or folder without a shell and without a process guard, and a file open verifies the recorded final file first.
17. The UI renders redacted stored and final URLs, never displays credential or header values, and treats a missing runtime or rejected command as a localized non-crashing state.

## Open risks

- Hosted CI has one successful run; manual platform acceptance, Windows install checks, and the Intel launch remain pending.
- Windows install, deep-link, tray, autostart, notification, credential, visual, and network checks require a Windows environment and the installed NSIS build.
- macOS x86_64 acceptance requires launching the uploaded Intel artifact on Intel hardware when available.
- Self-signed local signing can produce trust prompts; the identity is for local verification only and is not release signing.
- Windows notifications, and sometimes macOS prompts, can depend on an installed or stably signed application.
- The `macos-15-intel` runner is expected to remain available until August 2027; retirement would require cross-compilation or local Intel hardware and a revised risk record.
- macOS deep links depend on LaunchServices registration from a built application.
- Keychain and notification permission prompts can vary on unsigned local builds.
- The 24-hour soak has not run; only the short smoke and a 2-second ignored-path check have been exercised.
- Windows x64 sparse preallocation and offsets above 4 GiB remain a recorded pending compatibility result; the macOS large-file result does not prove the Windows behavior.
- The retained large-file evidence preserves the intermediate failing revision and the post-fix green transcript for the corrected suite; the accepted macOS arm64 transcript passes the large-file and platform-file suites, and the corrected suite remains the accepted large-file result.
- The Phase 0 manual platform acceptance matrix and the Phase 1 Windows compatibility and 24-hour soak gates remain pending; Phase 2 exit stays blocked until those gates are recorded or explicitly waived. No P0 failure was observed in the local arm64 or hosted CI evidence.
- The desktop download interface has automated component, Rust, and mock-IPC browser evidence only; a manual real-application local HTTP download and macOS and Windows visual acceptance remain pending. The dual-axis review repaired its initial blockers, and the first targeted recheck completed with a Spec PASS and residual Standards findings; the cycle-2 repair is applied and verified, but the final targeted recheck is pending, so no final review PASS is claimed.
- The context indexer discovers only TypeScript modules, so the Rust `engine`, `store`, `commands`, and `engine-harness` navigation entries are hand-maintained; a rebuild without reapplication drops them.
- The engine keeps full URLs and header values in memory only, so a process restart cannot replay a query-bearing URL or a custom header value; callers must re-supply them for a new session.
- Dynamic splitting, SQLite write contention, and crash recovery are exercised by short local tests and the soak harness, but long-run behavior under sustained load is not yet measured.
