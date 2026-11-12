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

## CI status

The GitHub Actions workflow is delivered with a frontend job, Rust quality gates on macOS and Windows, a three-target build matrix, and a macOS signing dry-run job. No real GitHub Actions run has been performed. The authorized remote configuration, branch push, and workflow execution are pending. CI is not green and is not claimed as green.

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
| macOS x86_64 artifact | Pending | Confirm the CI Intel artifact and, when hardware is available, launch it. |
| Windows x64 build and install | Pending | Build the NSIS artifact, install, and launch it. |
| Windows platform checks | Pending | Repeat single-instance, deep-link, tray, autostart, notification, credential, visual, and network checks. |
| Windows notifications from installed build | Pending | Test from the installed NSIS build rather than the release binary. |

## Skipped and deferred checks

- Hosted CI workflow execution, run identifier, and artifact names.
- Every manual acceptance item listed above.
- macOS x86_64 launch on Intel hardware.
- Windows build, install, launch, and all Windows platform checks.
- The startup network-log inspection and the manual light and dark visual review.
- macOS local signing trust-prompt behavior on the unsigned versus self-signed build.

Phase exit is not claimed. The baseline remains incomplete until the hosted CI run and the manual matrix produce recorded evidence, unless a later decision explicitly narrows the target matrix.

## Risk status

| Priority | Status | Detail |
| --- | --- | --- |
| P0 | No failure observed locally | No required behavior failed in the local macOS arm64 automated evidence. |
| P1 | Pending | Hosted CI, manual macOS arm64 acceptance, Windows build and platform checks, and the macOS x86_64 artifact are pending. |
| P2 | Open with mitigation | Self-signed certificate trust prompts are expected locally; Windows notifications require the installed NSIS build; the Intel runner is available only until its announced retirement; unsigned local builds can change keychain and notification prompt behavior; macOS deep links require LaunchServices registration from a built app. |

The P0 statement is limited to local automated evidence. It is not a phase-exit approval and does not cover the pending manual, hosted CI, Windows, or Intel results.
