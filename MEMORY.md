# Project memory

This file records the current standards and behavior of the OneGet desktop shell. Agent Notes record why decisions were made; keep both consistent in the same change.

## Stack and pins

- Node.js 24 (`>=24 <25`) with a committed `package-lock.json`; install with `npm ci`.
- React 19.3.1, React DOM 19.3.1, TypeScript 6.0.3, Vite 8.3.8, Tailwind 4.3.5, Vitest 5.1.0, Playwright 1.65.0, and ESLint 10.14.0.
- Tauri 2.12.5 with `@tauri-apps/api` and CLI 2.12.4; Rust edition 2021 on stable, with committed `src-tauri/Cargo.lock`.
- Rust boundaries: cap-std 4.0.3, keyring 4.2.0, ed25519-dalek 3.0.0, process-wrap 10.0.1, secrecy 0.10.3, zeroize 1.9.1, unicode-normalization 0.1, url 2.5.8.
- Every direct npm dependency is an exact version; `npm run check:pins` enforces this. Use `cargo --locked` for Rust gates.

## Repository and module boundaries

- `src/` is the React UI: shell, error boundary, design-system primitives, diagnostics, typed IPC wrapper, deep-link subscription, and shared types.
- `src-tauri/src/commands/` is the Tauri command surface plus directly testable service functions.
- `src-tauri/src/platform/` owns all target-specific behavior: paths, filenames, capability files, processes, secrets, deep links, tray, autostart, and notifications.
- `src-tauri/src/kernel/` owns canonical JSON and Ed25519 manifest verification only.
- `src-tauri/src/config.rs`, `logging/`, and `error.rs` own settings, JSON-line logs, and typed errors.
- `src-tauri/tests/` holds the Rust integration suites; `e2e/` holds Playwright checks; `scripts/` holds the pin check and local macOS signing helpers.
- `localdocs/` is ignored local reference material, not shipped product documentation.

## Platform-layer rule

Target-specific branches belong only in `src-tauri/src/platform/**` and platform-specific Tauri configuration. Shared modules call platform interfaces and never branch on the target operating system. New platform behavior needs an interface in the platform layer plus a test in the corresponding suite.

## Command and event contract

- Commands: `app_info`, `settings_get`, `settings_save`, `deep_link_last`, `diagnostics_secret_roundtrip`, `diagnostics_notification_status`, `diagnostics_send_notification`, `diagnostics_autostart_get`, `diagnostics_autostart_set`, and `app_graceful_quit`.
- Event: `app://deep-link` with `{ accepted, url?, error? }`.
- Rust payloads use camelCase and typed `{ code, message }` errors.
- The frontend calls only `src/lib/tauri.ts`. A missing Tauri runtime yields `runtime_unavailable`; the shell must not crash in a browser preview.
- Blocking keyring, file, and settings work runs through `spawn_blocking`; no command blocks the UI thread.

## Security invariants

- No analytics, telemetry, remote script sources, or remote connect sources. The production CSP is bundled-only except IPC; keep the no-telemetry rule when adding any network behavior.
- Deep links accept only `oneget://download` with exactly one `url` parameter, an `http` or `https` target, no embedded credentials, valid percent encoding, and at most 8 KiB.
- Credential values live only in the operating-system credential store. Never write them to settings, logs, diagnostics payloads, or frontend state; diagnostics return booleans only. Values are redacted in formatting and zeroized on drop.
- Managed file access goes through `CapDir` under a canonical root. Reject absolute paths, parent or current components, NUL bytes, and symbolic-link or junction escapes; atomic replace refuses link sources and destinations.
- Sanitize user-supplied filenames with the platform-aware sanitizer before use.
- Manifest verification uses canonical JSON with sorted keys and no whitespace, rejects duplicate keys and input above 1 MiB, removes the top-level signature before hashing, and uses `verify_strict`. The verifier does not install, download, or execute components.
- Child processes never use a shell; they run in a Unix process group or Windows Job Object with whole-tree kill and timeout.
- The main window hides on close; the tray provides Show and Quit; graceful quit records a shutdown event and exits with code 0.

## Settings, logging, and errors

- Settings use `schemaVersion: 1`, default `theme: system` and `logLevel: info`, preserve unknown top-level fields, save atomically through a same-directory temporary file and rename, back up corrupt documents to a `.corrupt` sibling, and treat a future schema as read-only.
- Logs are JSON lines with `timestamp`, `level`, `event`, and redacted `fields`. Redact credential-bearing URL parts, authorization and cookie headers, URL userinfo, proxy credential fields, and provider credential prefixes before serialization.
- Rotation defaults to 10 MiB per file and five files including the current file; a record is never split across files. Debug builds also write the redacted line to stderr.
- Errors are user-safe typed `AppError` values with stable codes and no internal paths, stack traces, or credential values.

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
- `npm run tauri build -- --bundles app` on macOS or `npm run tauri build -- --bundles nsis` on Windows
- `bash -n scripts/*.sh` before using the macOS signing helpers

## CI matrix

- Frontend job on Ubuntu: locked install, pin check, typecheck, lint, unit tests, build, and Playwright.
- Rust gates on `macos-15` and `windows-2025`: format, clippy with warnings denied, and tests.
- Build matrix: `macos-15` for `aarch64-apple-darwin` (`.app`), `macos-15-intel` for `x86_64-apple-darwin` (`.app`), and `windows-2025` for `x86_64-pc-windows-msvc` (NSIS), with uploaded artifacts.
- A macOS signing dry-run job creates an ephemeral certificate, imports it into a temporary keychain, signs and verifies a probe, cleans up, and checks fail-closed missing inputs.
- The workflow uses read-only permissions and concurrency cancellation. Hosted CI is not yet evidenced; do not claim it green without a real run identifier.
