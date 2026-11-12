# OneGet Architecture

This document describes the delivered OneGet desktop shell baseline. It covers the current layers, module boundaries, trust boundaries, platform differences, and verification baseline. It does not claim download, extension, video, updater, release, or telemetry capabilities that are not part of the shell.

## Goals and target platforms

The baseline provides a single-window desktop shell that:

- launches as one application instance and keeps a main window with a diagnostics surface;
- accepts a narrow `oneget://` deep-link contract and reports accepted or rejected results to the UI;
- integrates with a tray menu, login autostart, operating-system notifications, and the platform credential store;
- confines file access to capability-scoped directories and sanitizes user-supplied filenames;
- verifies signed component manifests without installing or executing components;
- writes structured, redacted, rotating logs and durable typed settings;
- builds with exact frontend and Rust dependencies and runs the same quality gates in CI;
- ships no remote script sources, no analytics, and no telemetry.

| Target | Architecture | Minimum or supported version |
| --- | --- | --- |
| macOS | arm64 and x86_64 | macOS 13 or later |
| Windows | x64 | Windows 10 22H2 or Windows 11 |

The shell is the current product surface. `localdocs/` is local reference material excluded from the bundle; it is not shipped product documentation.

## Repository layout

| Path | Responsibility |
| --- | --- |
| `src/` | React 19 and TypeScript 6 user interface, design-system primitives, diagnostics surface, typed command wrapper, and deep-link event subscription. |
| `src-tauri/src/` | Rust crate: Tauri runtime wiring, command layer, platform layer, manifest verifier, settings, logging, and typed errors. |
| `src-tauri/tests/` | Rust integration suites for paths, filenames, files, processes, secrets, logging, settings, manifests, and the shell contract. |
| `src-tauri/capabilities/` | Minimal Tauri capability for the main window. |
| `src-tauri/tauri.conf.json` | Product identity, window, CSP, deep-link scheme, and bundle configuration. |
| `scripts/` | Dependency-pin check and macOS local signing helpers. |
| `e2e/` | Playwright browser checks for the shell and design system. |
| `.github/workflows/ci.yml` | Frontend, Rust, build-matrix, and macOS signing dry-run jobs. |
| `assets/` and `src-tauri/icons/` | Source and generated application icons. |
| `extension/` | Extension placeholder; extension behavior is not implemented. |
| `localdocs/` | Ignored local reference material, not shipped documentation. |

## Layer and module boundaries

### React UI

`src/main.tsx` mounts `src/App.tsx`, which wraps `src/app/AppShell.tsx` in `src/app/ErrorBoundary.tsx`. The shell renders the product identity and diagnostics panel. `src/components/ui/` owns the Button, Input, and Radix Dialog primitives. `src/features/diagnostics/DiagnosticsPanel.tsx` exposes app information, settings, deep-link state, autostart, notifications, the diagnostic secret round-trip, and graceful quit.

`src/lib/tauri.ts` is the only command entry point. It returns a typed result, detects an absent Tauri runtime, and reports `runtime_unavailable` instead of throwing. `src/lib/events.ts` subscribes to the `app://deep-link` event and becomes inert without a runtime. `src/types/app.ts` freezes the command arguments and result shapes.

### Tauri runtime and command layer

`src-tauri/src/main.rs` delegates to `oneget_lib::run`. `src-tauri/src/lib.rs` registers the single-instance, deep-link, autostart, and notification plugins in that order, builds the tray, registers schemes, wires the deep-link event, and initializes state. Closing the main window hides it rather than exiting.

`src-tauri/src/app_state.rs` owns resolved paths, the settings store, the logger, service implementations, and the last deep-link result. `src-tauri/src/commands/mod.rs` contains the thin Tauri command wrappers and moves blocking work through `spawn_blocking`. `src-tauri/src/commands/app.rs` contains directly testable service functions and service traits.

The command and event contract is:

| Surface | Kind | Purpose |
| --- | --- | --- |
| `app_info` | command | Product name, version, and current log path. |
| `settings_get` | command | Current typed settings. |
| `settings_save` | command | Persist and return the stored settings. |
| `deep_link_last` | command | Most recently handled deep-link result. |
| `diagnostics_secret_roundtrip` | command | Store, read, and delete a generated diagnostic value; returns a boolean. |
| `diagnostics_notification_status` | command | Current notification permission. |
| `diagnostics_send_notification` | command | Send a diagnostic notification. |
| `diagnostics_autostart_get` | command | Current autostart registration state. |
| `diagnostics_autostart_set` | command | Enable or disable autostart and return the resulting state. |
| `app_graceful_quit` | command | Write a shutdown record and exit with code 0. |
| `app://deep-link` | event | `{ accepted, url?, error? }` for accepted or rejected links. |

Rust payloads serialize camelCase. Errors use a typed `{ code, message }` shape.

### Platform layer

`src-tauri/src/platform/` is the only place where target-specific branches live. It owns:

- application paths for data, cache, logs, downloads, and backups;
- deterministic filename sanitization for macOS and Windows rules;
- `CapDir`, the canonical capability root for asynchronous create, write, read, sync, metadata, and atomic replace;
- `ChildGuard`, which owns a Unix process group or Windows Job Object and supports whole-tree termination and timeouts;
- the keyring-backed secret store and the in-memory fake;
- deep-link parsing and runtime scheme registration;
- the tray, autostart, and notification plugin adapters.

Shared modules call these interfaces and do not branch on the target operating system. The platform layer exposes typed `PlatformError` values with stable codes.

### Kernel manifest verifier

`src-tauri/src/kernel/manifest.rs` canonicalizes JSON by sorting object keys recursively and emitting no insignificant whitespace. It rejects duplicate keys, non-object roots, malformed input, and payloads above 1 MiB. Verification removes the top-level `signature` field, decodes the Ed25519 signature and public key, and calls `verify_strict`. The module is a verification primitive only: it contains no installer, downloader, signing key material, or component execution.

### Configuration, logging, and errors

`src-tauri/src/config.rs` owns schema version 1 settings: theme and log level with system defaults. Loading preserves unknown top-level fields, creates defaults when absent, backs up corrupt documents to a `.corrupt` sibling, and treats a document from a newer schema as read-only. Saves use a same-directory temporary file, sync, and rename.

`src-tauri/src/logging/` owns JSON-line logging, redaction, and size rotation. `src-tauri/src/error.rs` owns the typed application error. Details are in the baseline section below.

### Tests and quality tooling

Frontend behavior tests live beside components and in `src/features/`. Browser checks live in `e2e/`. Rust integration suites live in `src-tauri/tests/`. `scripts/check-direct-dependency-pins.mjs` rejects floating direct npm versions. The macOS signing helpers are local verification tools, not release signing.

## Trust boundaries

### WebView and content security policy

The WebView loads bundled assets only. The production CSP allows scripts, styles, fonts, images, and media from the application origin and permits IPC connections only through `ipc:` and `http://ipc.localhost`. The development CSP additionally allows the local Vite server. There is no remote script source, no remote connect source, no analytics dependency, and no telemetry endpoint.

### Deep links

Deep links are untrusted input. The parser accepts only a `oneget://download` link with exactly one `url` parameter. The target must parse as `http` or `https`, must not contain embedded credentials, must use valid percent encoding, and the whole link is limited to 8 KiB. Missing, duplicate, malformed, non-http, credentialed, and oversized links produce typed rejections. Windows registers schemes at runtime; macOS relies on the built bundle registration.

### Credential storage

Credential values are held in the operating-system credential store through a `SecretStore` interface. Values are wrapped so formatting shows a redaction marker, and the wrapper zeroizes on drop. The diagnostic round-trip command stores, reads, compares, and deletes a generated value but returns only a boolean. Automated tests use an in-memory store and never touch the real keychain. Settings and logs never contain credential values.

### Capability-scoped files

Every managed write goes through `CapDir`. The root is created and canonicalized before use. Input paths must be relative, contain no NUL bytes or backslashes, and contain only normal components; absolute paths, parent components, and current components are rejected. Existing targets are canonicalized and rejected when they resolve outside the root. Writes and creates refuse a symbolic-link target, atomic replace refuses symbolic-link sources and destinations, and parent traversal is checked before creation. Asynchronous operations run on the blocking pool.

### Component manifests

A manifest signature is verified over canonical JSON with the top-level signature removed. Duplicate keys are rejected at every nesting level so the parsed and signed views cannot diverge. Signatures and keys must be exact hex lengths, and verification uses `verify_strict`. The verifier does not install, download, or execute anything.

### Child processes

No production code invokes a shell. A child is spawned as a Unix process-group leader or inside a Windows Job Object with kill-on-drop. `kill_tree` terminates the whole tree and reaps the top-level child; `wait_timeout` kills the tree and returns a typed timeout. The drop guard signals a still-running tree.

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
| Bundle format | `.app` | NSIS installer |

The non-macOS, non-Windows path code is a development fallback using XDG locations; it is not a shipped target.

## Logging, error, and configuration baseline

- Log records are JSON lines with an epoch-millisecond `timestamp`, lowercase `level`, redacted `event`, and recursively redacted `fields`.
- The default rotation is 10 MiB per file and five files including the current one. Records are never split across files. Debug builds also mirror the redacted line to stderr.
- The log level is part of settings and filters records above the configured verbosity.
- `AppError` serializes as `{ code, message }`, with stable codes such as `settings_read_only` and `settings_serialize`.
- Settings start at `schemaVersion: 1` with `theme` (`system`, `light`, `dark`) and `logLevel` (`error`, `warn`, `info`, `debug`, `trace`).
- Unknown settings fields survive a save. A future schema is never overwritten. A corrupt document is backed up before defaults are restored.
- There is no database, migration, or other persistent product data in the baseline.

## Test and CI baseline

Local macOS arm64 evidence covers `npm ci`, the exact-pin check, typecheck, lint, 20 Vitest tests, the production build, and 8 Playwright checks across light and dark projects. Rust evidence covers `cargo check`, `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and `cargo test`, with 50 integration tests across nine suites. The Tauri application bundle builds locally.

The CI workflow defines a frontend job, Rust quality gates on `macos-15` and `windows-2025`, a build matrix for `aarch64-apple-darwin`, `x86_64-apple-darwin`, and `x86_64-pc-windows-msvc`, and a macOS signing dry-run job that creates, imports, signs, verifies, and cleans up an ephemeral identity and fails closed on missing inputs. The workflow is delivered but has not run on the authorized remote; hosted CI, manual platform acceptance, Windows checks, and the Intel artifact remain pending evidence.

## Security invariants

1. The production WebView has no remote script or connect source, and the product carries no analytics or telemetry.
2. Credential values never appear in logs, settings, diagnostic payloads, or frontend state.
3. Managed filesystem access is confined to a canonical capability root; traversal, absolute paths, NUL bytes, and symbolic-link or junction escapes are rejected.
4. Deep links accept only one `oneget://download` link with one validated `http` or `https` target, no embedded credentials, valid encoding, and an 8 KiB limit.
5. Manifest verification uses canonical JSON, rejects duplicate keys, excludes the top-level signature, and uses strict Ed25519 verification; it never installs or executes a component.
6. Child processes never use a shell and are terminated as a whole tree on timeout or drop.
7. Platform-specific code stays under `src-tauri/src/platform/`; shared modules do not branch on the target operating system.
8. Settings and logs are redacted or typed and never become a secondary secret store.
9. The main window hides on close, the tray provides Show and Quit, and graceful quit records a shutdown event before exiting with code 0.

## Open risks

- Hosted CI and every manual platform acceptance item are pending.
- Windows build, install, deep-link, tray, autostart, notification, credential, visual, and network checks require a Windows environment and the installed NSIS build.
- macOS x86_64 acceptance requires the CI Intel artifact and, when available, Intel hardware.
- Self-signed local signing can produce trust prompts; the identity is for local verification only and is not release signing.
- Windows notifications, and sometimes macOS prompts, can depend on an installed or stably signed application.
- The `macos-15-intel` runner is expected to remain available until August 2027; retirement would require cross-compilation or local Intel hardware and a revised risk record.
- macOS deep links depend on LaunchServices registration from a built application.
- Keychain and notification permission prompts can vary on unsigned local builds.
- No P0 failure was observed in the local arm64 evidence, but phase exit is incomplete until hosted CI and the manual matrix are recorded.
