# Feature navigation

| Feature | Owner | Entry | Responsibility | Locate |
| --- | --- | --- | --- | --- |
| e2e | shared | e2e/support/mock-ipc.ts | Discovered typescript module at e2e | ai-workflow context locate --feature e2e --verify |
| extension | shared | extension/chromium/background.ts | Chromium and Firefox MV3 browser-capture extensions: shared browser-agnostic controller and typed adapter, per-browser m | ai-workflow context locate --feature extension --verify |
| root | shared | eslint.config.js | Discovered typescript module at . | ai-workflow context locate --feature root --verify |
| scripts | shared | scripts/check-direct-dependency-pins.mjs | Discovered typescript module at scripts | ai-workflow context locate --feature scripts --verify |
| src | shared | src/App.tsx | Discovered typescript module at src | ai-workflow context locate --feature src --verify |
| api | shared | src-tauri/src/api/auth.rs | Loopback browser-integration API: fixed admission guards with exact Chromium and Firefox extension origins, capture prot | ai-workflow context locate --feature api --verify |
| engine | shared | src-tauri/src/engine/scheduler.rs | Headless download engine: probe, transport, planner, conflict-aware part files, workers, scheduler, service actor, retry | ai-workflow context locate --feature engine --verify |
| store | shared | src-tauri/src/store/db.rs | Durable SQLite task store: connection setup, schema 1 and 2 migrations, per-task destinations, part names, minimized fin | ai-workflow context locate --feature store --verify |
| commands | shared | src-tauri/src/commands/downloads.rs | Tauri command surface: typed download, settings, diagnostics, pairing, and quit commands plus directly testable service  | ai-workflow context locate --feature commands --verify |
| engine-harness | shared | src-tauri/src/bin/engine_harness.rs | Command-line harness for adding, running, inspecting, pausing, resuming, and cancelling one task | ai-workflow context locate --feature engine-harness --verify |
