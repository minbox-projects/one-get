# Agent Note: Download engine and durable task store

Status: implemented

English | [中文](2026-11-12-download-engine-task-store.zh.md)

## Problem

OneGet needed a headless download engine and a durable task store before any desktop download interface could be built. The engine had to probe remote files safely, plan bounded segments, survive a process crash, and finalize files atomically without exposing query tokens, cookies, proxy credentials, or custom header values. Without a recorded decision, later work could treat the part file as optional, replay secret header values from the store, trust a probe's Range claim, or write a final file directly and leave partial output.

## Decision

The delivered engine is a Rust library under `src-tauri/src/engine/` with a SQLite store under `src-tauri/src/store/` and a command-line harness at `src-tauri/src/bin/engine_harness.rs`. It extends, rather than replaces, the [OneGet desktop shell baseline](2026-11-11-oneget-desktop-shell-baseline.md): the shell owns the process, platform, logging, settings, and capability boundaries, and the engine calls the platform `CapDir` for every managed file operation.

The part file is the byte truth and the SQLite segment rows are the progress truth. Task creation sanitizes the caller-supplied filename with the platform-aware sanitizer before it reaches the store, and a task owns one `<sanitized filename>.oneget.part` beside its destination. Workers write at absolute `u64` offsets, a known total is preallocated sparsely through `PartFile::preallocate` and the capability layer, and a full device maps to `DISK_FULL` and pauses the task. Part existence and removal use the validated `CapDir::exists` and `CapDir::remove_file` operations, so a part that resolves outside the destination root is never inspected or removed. The store uses schema version 1 with `tasks`, `segments`, and `schema_meta`, WAL journaling, foreign keys, a five-second busy timeout, a `0600` database on Unix, a pre-migration backup, and a refusal of a newer schema. Segment rows use half-open ranges that stay non-overlapping and gap-free, 1 to 64 connections, and a 1 MiB minimum except for a small file or a final remainder.

The scheduler claims the oldest queued task and counts only connecting, downloading, and finalizing tasks against its limit. More than one engine instance may share the database; a control applied through one instance is reconciled from the reloaded stored row by the other, so a concurrent pause ends paused and a concurrent cancel ends cancelled instead of failed or `INVALID_STATE`. A cancel that wins during finalization removes the part without leaving a final file, and a completion that loses the state race removes the file it created. Pause, resume, cancel, retry, and startup recovery are idempotent within their defined states. Resume re-probes the remote metadata and rejects a changed ETag, Last-Modified, or total with `FILE_CHANGED`; a missing part fails with `PART_MISSING`. Recovery requeues interrupted tasks with a part, re-finalizes a finalizing task, removes only a cancelled task's part, and preserves paused and failed parts.

Finalization stops workers, syncs the part, streams SHA-256, persists the hash before the rename, verifies any expected hash, chooses a unique name on collision, and replaces through the capability layer. A probe result is advisory: the engine revalidates the first range before the full pool, falls back to one connection when a fresh task receives a `200`, and fails with `RANGE_IGNORED` after bytes exist without writing a full body at a nonzero offset.

Retries are bounded to five attempts with exponential backoff and full jitter, an integer `Retry-After` is honored exactly, and an HTTP-date value uses the fallback. Rate limiting is a shared byte leaky bucket for each task and optionally for the whole engine. The HTTP proxy refuses embedded userinfo and keeps optional basic authentication in memory; Netscape cookies are scoped by host, path, expiry, and secure flag and reloaded at each task start; safe custom headers are accepted while forbidden and CRLF-bearing headers are rejected.

Privacy and crash behavior are recorded together. Header values, cookie values, proxy credentials, authorization values, and URL query or fragment tokens never enter the store or the log. Safe header names may be stored with empty values, the stored URL strips userinfo, query, and fragment, and logs carry only event names, identifiers, byte counts, and error codes. macOS uses sparse `set_len`; Windows attempts `FSCTL_SET_SPARSE` and reports an explicit fallback. The short smoke runs in the normal suite, and the ignored soak repeats the lifecycle against local fixtures. The engine pins its retry-jitter and chunk-buffering dependencies exactly, resolves them through the committed lockfile, and runs every gate with `--locked`.

## Alternatives considered

- Keeping progress only in part-file metadata and a sidecar journal was declined because multi-segment progress and queue state need the atomic snapshots and queries that the store provides.
- Persisting full header values for exact replay was declined because a privacy failure in the store is permanent; values stay in memory and a later session must re-supply them.
- Persisting query-bearing URLs or signed tokens was declined for the same reason; only the minimized URL is durable.
- Starting the full worker pool on the probe result was declined because a server can report Range support and later return `200`; the first-range revalidation and the worker-level guard are retained.
- Extending the part file with a direct standard-library length change outside the capability layer was declined because it would bypass validated path handling and the platform sparse outcome; `PartFile::preallocate` keeps one capability-checked path and maps a full device to `DISK_FULL`.
- Rejecting a control that arrives through a second engine connection with `INVALID_STATE` was declined because two engines over one database are a supported configuration; the stored row is the authority and the running instance reconciles to the pause or cancel instead of reporting a failure.
- Writing directly to the final name was declined because a crash or checksum failure would leave a partial final file; the part file plus capability-scoped replace keeps the final name atomic.
- Fully allocating the logical length on Windows was declined because it would consume disk for sparse ranges; the sparse attempt keeps the explicit fallback result.

## Consequences

Later desktop work must call this engine and store rather than duplicate download or persistence logic. Any interface that adds a task must pass values it can re-supply after a restart, because header values and query-bearing URLs are not replayable from the store. The store remains the progress authority and the part file remains the byte authority; a completed task always has a final file and a matching stored hash.

The design accepts a known limitation: a fresh process cannot reconstruct a secret header value or a signed URL from the database, so resume after a restart relies on the caller re-supplying them. The engine also makes no system-proxy discovery, SOCKS5, browser-cookie extraction, HuggingFace request, telemetry, or remote reporting attempt.

- Platform-specific allocation and replacement stay in `src-tauri/src/platform/`; the engine does not branch on the target operating system outside that boundary.
- New network settings belong in a later change that supplies the added proxy, cookie, rate, or header inputs through the existing engine methods.
- The 24-hour soak and a Windows x64 run remain required evidence before the download capability can be treated as fully accepted.
