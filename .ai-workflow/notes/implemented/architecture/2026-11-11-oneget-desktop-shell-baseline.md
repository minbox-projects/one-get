# Agent Note: OneGet desktop shell baseline

Status: implemented

English | [中文](2026-11-11-oneget-desktop-shell-baseline.zh.md)

## Problem

OneGet needed a trustworthy desktop foundation before download, extension, video, or release features could be built. The shell had to launch as one window on macOS and Windows, expose a diagnostics surface, accept deep links safely, integrate with the tray, autostart, notifications, and the platform credential store, and establish filesystem and logging boundaries that later features cannot bypass. Without a recorded decision, later work could re-centralize platform branches, write outside capability roots, log credential material, or add remote assets and telemetry.

## Decision

The delivered baseline is a Tauri 2 desktop application with a React 19 and TypeScript 6 user interface, a Vite 8 build, and a Rust backend pinned through Cargo. The React layer owns the shell, design-system primitives, and diagnostics surface; it calls the backend only through the typed command wrapper and treats an absent Tauri runtime as `runtime_unavailable` instead of crashing. The Tauri command layer owns app information, settings, deep-link state, diagnostic secret round-trip, notification and autostart diagnostics, and graceful quit. Events use `app://deep-link`, and Rust payloads use camelCase with typed `{ code, message }` errors.

All target-specific behavior is confined to the platform layer: application paths, filename sanitization, capability-scoped files, guarded child processes, secret storage, deep links, tray, autostart, and notifications. Shared modules depend only on those interfaces. File writes go through a `CapDir` rooted at a canonical directory and reject absolute paths, parent traversal, NUL bytes, symbolic-link targets, and link escapes; the asynchronous API runs through `spawn_blocking`. Filenames are normalized and sanitized for macOS or Windows rules before use.

The trust boundaries are explicit. The WebView uses a restrictive content security policy with no remote script or connect sources; assets are bundled and there is no analytics or telemetry. Deep links are untrusted input and are limited to a single `oneget://download` link with one `http` or `https` target, no embedded credentials, valid percent encoding, and an 8 KiB limit. Credentials live only in the operating-system credential store through typed secret interfaces, values are redacted in formatting and zeroized on drop, and the diagnostics command reports booleans rather than values. Component manifests are verified with canonical JSON and Ed25519 `verify_strict`; the verifier excludes the top-level signature, rejects duplicate keys and oversized input, and adds no installer, downloader, or execution path.

Logging writes JSON lines with a timestamp, level, event, and redacted fields, rotates at 10 MiB across five files, and mirrors to stderr in debug builds. Settings use schema version 1, preserve unknown fields, save atomically, back up corrupt documents, and treat a future schema as read-only. No credential value is written to settings or logs.

## Alternatives considered

A plain web application or an Electron shell was declined because it would widen the trust boundary with a remote-capable runtime and a larger update surface. Using ambient filesystem APIs instead of a capability root was declined because path validation would depend on every caller. Storing credentials in settings or a local encrypted file was declined because the operating-system credential store already provides the platform boundary and user control. Implementing download, extension, video, update, and release pipelines in this change was declined so the shell could establish and verify its boundaries first.

## Consequences

Later features must call the recorded commands and events, keep target-specific code inside the platform layer, and route every file write through the capability layer. New network or asset behavior must preserve the no-telemetry rule and the restrictive content security policy. Credential values must stay out of settings, logs, diagnostics payloads, and frontend state. Deep-link handling must keep rejecting malformed, credentialed, duplicate, non-http, and oversized links.

The delivered local evidence covers frontend quality gates, Rust unit and integration tests, capability and link-escape tests, manifest accept and reject cases, filename adversarial cases, process-tree termination, secret-store fakes, log redaction and rotation, settings recovery, deep-link parsing, and a local self-signed certificate create, import, verify, and cleanup flow with fail-closed missing inputs. Hosted CI, manual platform acceptance, and the Windows and Intel checks remain deferred; the baseline does not claim those results. Download, extension, video, release signing, notarization, updater, and telemetry work remain outside this baseline.
