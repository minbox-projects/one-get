# Feature navigation

| Feature | Owner | Entry | Responsibility | Locate |
| --- | --- | --- | --- | --- |
| e2e | shared | e2e/app-shell.spec.ts | Discovered typescript module at e2e | ai-workflow context locate --feature e2e --verify |
| root | shared | eslint.config.js | Discovered typescript module at . | ai-workflow context locate --feature root --verify |
| scripts | shared | scripts/check-direct-dependency-pins.mjs | Discovered typescript module at scripts | ai-workflow context locate --feature scripts --verify |
| src | shared | src/App.tsx | Discovered typescript module at src | ai-workflow context locate --feature src --verify |
| engine | shared | src-tauri/src/engine/scheduler.rs | Headless download engine: probe, transport, planner, part file, workers, scheduler, retry, rate limiting, cookies, and s | ai-workflow context locate --feature engine --verify |
| store | shared | src-tauri/src/store/db.rs | Durable SQLite task store: connection setup, schema migration, task and segment records, and progress persistence | ai-workflow context locate --feature store --verify |
| engine-harness | shared | src-tauri/src/bin/engine_harness.rs | Command-line harness for adding, running, inspecting, pausing, resuming, and cancelling one task | ai-workflow context locate --feature engine-harness --verify |
