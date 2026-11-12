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
