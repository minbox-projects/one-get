# Feature navigation

| Feature | Owner | Entry | Responsibility | Locate |
| --- | --- | --- | --- | --- |
| e2e | shared | e2e/support/mock-ipc.ts | Discovered typescript module at e2e | ai-workflow context locate --feature e2e --verify |
| root | shared | eslint.config.js | Discovered typescript module at . | ai-workflow context locate --feature root --verify |
| scripts | shared | scripts/check-direct-dependency-pins.mjs | Discovered typescript module at scripts | ai-workflow context locate --feature scripts --verify |
| src | shared | src/App.tsx | Discovered typescript module at src | ai-workflow context locate --feature src --verify |
| engine | shared | src-tauri/src/engine/scheduler.rs | Headless download engine: probe, transport, planner, part file, workers, scheduler, service actor, retry, rate limiting, | ai-workflow context locate --feature engine --verify |
| store | shared | src-tauri/src/store/db.rs | Durable SQLite task store: connection setup, schema 1 and 2 migrations, per-task destinations, task and segment records, | ai-workflow context locate --feature store --verify |
| commands | shared | src-tauri/src/commands/downloads.rs | Tauri command surface: typed download, settings, diagnostics, and quit commands plus directly testable service functions | ai-workflow context locate --feature commands --verify |
| engine-harness | shared | src-tauri/src/bin/engine_harness.rs | Command-line harness for adding, running, inspecting, pausing, resuming, and cancelling one task | ai-workflow context locate --feature engine-harness --verify |
