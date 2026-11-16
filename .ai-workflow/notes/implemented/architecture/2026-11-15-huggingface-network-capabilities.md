# Agent Note: HuggingFace downloads, network transport, and cookie sources

Status: implemented

English | [中文](2026-11-15-huggingface-network-capabilities.zh.md)

## Problem

OneGet could download ordinary HTTP files and video, but it had no HuggingFace resolver, no global proxy selection, and only per-download cookie and proxy inputs behind a read-only network placeholder. A user with a gated HuggingFace file, a corporate system proxy, a SOCKS5 proxy, a slow origin, or a browser session needed manual workarounds. Without a recorded decision, later work could attach an access token to a CDN host, persist browser cookies in the task store, merge cookie sources, treat a PAC or WPAD configuration as a usable proxy, or change the capture protocol in a way that breaks an older desktop or extension.

## Decision

HuggingFace recognition is limited to `huggingface.co`, `www.huggingface.co`, `hf.co`, and `www.hf.co`, with model, `/datasets/`, and `/spaces/` file paths under `/resolve/` or `/blob/`. A `/blob/` path normalizes to `/resolve/`, any other resolver path is `HF_NOT_A_FILE` before a task exists, and CDN or unrelated hosts stay ordinary URLs so the token is never attached to them. The resolver returns a signed CDN URL; the access token is sent only to resolver-host requests, and the first redirect that leaves the resolver hosts is returned without contacting the CDN.

The initial resolution may retry once after a 401, and that retry is not a signature refresh; at most ten resolver redirects are followed. After a successful initial resolution, a CDN 403 may trigger up to three signature refreshes, so a fully exercised task contacts the resolver at most four times. A 401 without usable authorization is `HF_AUTH`; an expired CDN signature is refreshed without re-downloading completed bytes, the refresh count starts at 0 and reaches 3 at the limit, an exhausted limit is `HF_SIGNATURE`, and a refresh that reports unauthorized is `HF_AUTH`. The saved connection cap, 1 to 64 with a default of 8, limits a resolver task at start, a 429 or an ignored Range reduces the task to one connection for the remainder of the run, and `hf.db` at schema version 1 keeps repository, file, revision, cap, effective connections, refresh count, and downgrade state so a paused task can resume after restart by re-resolving from its stored resolver URL.

Proxy selection has three modes. `direct` is the default and ignores stored credentials and the system configuration. `system` reads the macOS `scutil --proxy` or Windows Internet Settings values, prefers the HTTPS endpoint then HTTP, and never carries credentials; PAC, WPAD, auto-detect, or an unreadable configuration reports a stable reason and connects directly. `manual` supports `http`, `https`, and `socks5` with a non-empty host, a port from 1 to 65535, and optional basic credentials held in the operating-system credential store. The proxy URL never embeds userinfo, credentials are deleted when direct or system mode is saved, and the effective proxy applies to the HTTP engine, video service, and component downloader.

Connect and read timeouts are configurable with defaults of 15 and 30 seconds, retry attempts are configurable from 1 to 10 with a default of 5, and a configured non-empty User-Agent without line breaks is the fallback for HTTP and HuggingFace requests without a per-task value. These options apply to tasks that start or continue after the save; an already running child keeps its captured proxy and transport configuration.

A task has exactly one cookie source, never a merged list. A per-task or default Netscape cookie file wins, then captured browser cookies. Cookie files match host, include-subdomains, path, secure, and expiry, are reloaded at each start or resume, and are recomputed per redirect hop. Captured browser cookies are memory-only, limited to 100 per item and 8 KiB combined, URL-scoped at capture and use, and single-use through an opaque handle; an unknown or consumed handle is `INVALID_COOKIE_HANDLE`, a scope mismatch is `COOKIE_SCOPE`, and an over-limit list is `COOKIE_LIMIT`.

Capture protocol version 2 adds that optional cookie list while the desktop still accepts version 1 captures without cookies. The extension requests the `cookies` permission in both packages, but reads and attaches cookies only when the default-off `sendCookies` setting is enabled, and only cookies scoped to the captured URL. Cookie values never enter settings, the task database, the HuggingFace database, or the log, and the main-world observer never reads cookies.

Settings stay at schema version 2 with only optional fields, so a build without this change loads defaults, preserves unknown fields, and keeps working. The task store stays at schema version 3, `hf.db` is a separate schema version 1 file, and proxy credentials and the HuggingFace token stay in the credential store. Rolling back keeps settings readable and tasks listable, leaves `hf.db` as unused metadata, and returns capture behavior to version 1 when the extension package is rolled back; an older desktop rejects a version 2 capture with `PROTOCOL_MISMATCH`.

This record partially supersedes the network statements in the [Download engine and durable task store](2026-11-12-download-engine-task-store.md), [Desktop download interface and engine service](2026-11-13-desktop-download-ui.md), [Signed components and the video pipeline](2026-11-13-component-video-pipeline.md), [Loopback browser capture and browser extension integration](2026-11-13-local-api-browser-capture.md), and [Firefox extension parity through a shared controller](2026-11-13-firefox-extension-parity.md) records. Those records stay active for the engine, store, interface, video, pairing, capture, and Firefox decisions they still own, and each one cross-links this record.

## Alternatives considered

- Sending the HuggingFace token to the CDN or to every host was declined because the signed CDN URL needs no token and a wider token scope would leak a long-lived credential to unrelated infrastructure.
- Keeping only per-download proxy and cookie inputs was declined because system and corporate proxies, a shared default cookie file, and a browser session are ordinary daily needs that a per-task field cannot express.
- Storing browser cookies or proxy credentials in settings or the task database was declined because those values are secrets with a browser or operating-system lifetime; the credential store and memory-only handles keep them out of durable product data.
- Merging a cookie file and captured browser cookies was declined because two sources can disagree on domain, path, and expiry, and a single explicit precedence is easier to reason about and to test.
- Trusting a PAC or WPAD configuration by evaluating it was declined because it would add a script engine and a remote discovery dependency; an explicit unavailable reason and a direct connection are the smaller, auditable boundary.
- Reading browser cookies on every capture without an opt-in was declined because it would widen the extension permission's effect and surprise users; the default-off toggle limits cookie access to an explicit choice.
- Storing HuggingFace metadata in the task database was declined because details are advisory and a missing or corrupt metadata file must not affect task listing or control; a separate `hf.db` keeps that failure independent.
- Adding the new fields under a new settings schema version or rewriting the task store was declined because optional fields and a separate metadata file keep older readers working and make rollback non-destructive.

## Consequences

Later network work must preserve the resolver-host token boundary, the signed-URL refresh and connection cap, the single-source cookie precedence, the memory-only captured-cookie handle, the three proxy modes with the stable PAC and unreadable reasons, and the configurable transport options. The extension cookie permission may be used only behind the default-off opt-in and only for URL-scoped cookies; adding another capture field or a protocol version requires a compatible additive change and the same credential exclusion.

The delivered verification uses local HuggingFace, proxy, SOCKS5, system-proxy, and cookie fixtures, so it does not prove a public HuggingFace host, a real corporate proxy, a real browser cookie population, or Windows behavior. Those items remain manual and pending, and automated green results are not a phase-exit approval.

- A missing or corrupt `hf.db` keeps tasks listable and only loses HuggingFace details; deleting it does not remove tasks, files, settings, or pairings.
- The effective proxy and transport settings apply to tasks that start or continue after the save, while a running child keeps the configuration it captured at spawn.
- The context indexer discovers only TypeScript modules, so the Rust `engine`, `store`, `platform`, `commands`, `api`, `kernel`, `video`, and harness navigation entries stay hand-maintained.
