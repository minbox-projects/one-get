# Feature navigation

| Feature | Owner | Entry | Responsibility | Locate |
| --- | --- | --- | --- | --- |
| e2e | shared | e2e/support/mock-ipc.ts | Discovered typescript module at e2e | ai-workflow context locate --feature e2e --verify |
| extension | shared | extension/chromium/background.ts | Discovered typescript module at extension | ai-workflow context locate --feature extension --verify |
| root | shared | eslint.config.js | Discovered typescript module at . | ai-workflow context locate --feature root --verify |
| scripts | shared | scripts/check-direct-dependency-pins.mjs | Discovered typescript module at scripts | ai-workflow context locate --feature scripts --verify |
| src | shared | src/App.tsx | Discovered typescript module at src | ai-workflow context locate --feature src --verify |
| src-tauri | shared | src-tauri/tests/fixtures/hls/aes128/segment0.ts | Discovered typescript module at src-tauri | ai-workflow context locate --feature src-tauri --verify |
| api | shared | src-tauri/src/api/auth.rs | Loopback browser-integration API: fixed admission guards with exact Chromium and Firefox extension origins, capture prot | ai-workflow context locate --feature api --verify |
| engine | shared | src-tauri/src/engine/scheduler.rs | Headless HTTP download engine: proxy- and User-Agent-aware transport, probe, planner, conflict-aware part files, HTTP-on | ai-workflow context locate --feature engine --verify |
| store | shared | src-tauri/src/store/db.rs | Durable SQLite task store: schema 1 to 3 migrations, task kind and video records, destinations, part names, minimized fi | ai-workflow context locate --feature store --verify |
| commands | shared | src-tauri/src/commands/app.rs | Tauri command surface: typed download, video, component, settings, HuggingFace token, proxy credential, diagnostics, pai | ai-workflow context locate --feature commands --verify |
| kernel | shared | src-tauri/src/kernel/archive.rs | Signed component catalog and installation: strict Ed25519 catalog policy, verified cache, bounded artifact download, dec | ai-workflow context locate --feature kernel --verify |
| video | shared | src-tauri/src/video/control.rs | Guarded video pipeline: yt-dlp probe and quality model, process containment and bounds, one-at-a-time service, task-owne | ai-workflow context locate --feature video --verify |
| engine-harness | shared | src-tauri/src/bin/engine_harness.rs | Command-line harness for adding, running, inspecting, pausing, resuming, and cancelling one HTTP task | ai-workflow context locate --feature engine-harness --verify |
| kernel-manifest | shared | src-tauri/src/bin/kernel_manifest.rs | Maintainer tool for signed component catalogs: keygen, sign, and verify without storing or logging private key material | ai-workflow context locate --feature kernel-manifest --verify |
| video-fixture | shared | src-tauri/src/bin/video_fixture.rs | Cross-platform fixture process for valid probe output, progress, merge, failure, hang, flood, grandchild, and missing-ou | ai-workflow context locate --feature video-fixture --verify |
| platform | shared | src-tauri/src/platform/autostart.rs | Target-specific paths, filenames, capability files, component preparation, shell-free opener, process containment, secre | ai-workflow context locate --feature platform --verify |
