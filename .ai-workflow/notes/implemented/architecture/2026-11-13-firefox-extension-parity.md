# Agent Note: Firefox extension parity through a shared controller

Status: implemented

English | [中文](2026-11-13-firefox-extension-parity.zh.md)

## Problem

The browser capability was delivered for Chromium alone: the loopback API accepted only a `chrome-extension://` origin, and the controller, content script, observer, and takeover trigger lived inside the Chromium package. Firefox offers the same capture and prefill capability but differs in the details that decide correctness. Its background is an event page that can be suspended, its download API uses promises, `downloads.onCreated` replaces `onDeterminingFilename`, an absolute download filename must become a safe basename, `totalBytes: -1` means unknown length, and its extension origin is a `moz-extension://` UUID. Extending the capability without duplicating the decision logic or weakening the existing API, pairing, and capture boundaries required a record of its own; the [loopback browser capture note](2026-11-13-local-api-browser-capture.md) remains the owner of the pairing, capture, prefill, and privacy decisions it still describes.

## Decision

The shared package owns the browser-agnostic behavior. `extension/shared/background.ts` exports one controller that decides pairing, observation, takeover, prefill, resource bounds, and failure state; `extension/shared/browser.ts` is a small typed adapter that normalizes callback-style Chromium and promise-style Firefox messaging, active-tab lookup, local and session storage, and response headers. The isolated content script, MAIN-world observer, and overlay styles moved into `extension/shared`. `extension/chromium/background.ts` became a thin installer that registers the existing service-worker listeners, injects Chrome dependencies, and keeps the Chromium-only `onDeterminingFilename` flow; `extension/firefox/background.ts` is an event-page adapter that registers runtime, response, downloads, navigation, and tab listeners synchronously, injects the promise-based `browser.*` API, and pairs with the label `Firefox extension`. The popup imports the shared surface, and the shared controller accepts the pair label through its dependencies rather than through a protocol change.

The Firefox package is a separate MV3 output with `background.scripts` instead of a service worker, the stable Gecko ID `oneget@oneget.app`, `strict_min_version` `140.0`, and `data_collection_permissions.required: ["none"]`. It requests the same five permissions, the same HTTP(S) and loopback host permissions, the same bundled-only extension-page CSP, and the same popup, icons, isolated content script, and MAIN-world observer as Chromium. Firefox for Android, signing, and publishing stay out of scope, and the desktop floor is not lowered.

The API origin boundary now recognizes both exact extension schemes. `is_browser_extension_origin` accepts `chrome-extension://` plus 32 `a`–`p` characters or `moz-extension://` plus a lowercase 8-4-4-4-12 UUID. Admission order, body and header limits, rate limits, typed errors, content-type checks, bearer extraction, and the exact token-bound origin are unchanged, and no wildcard CORS header is added. A Firefox token is bound to its own exact origin, so a token presented with the other scheme is rejected.

Firefox takeover is evaluated in `downloads.onCreated`, never in `onDeterminingFilename`. The adapter maps a Firefox download to the shared model by reducing an absolute path to a sanitized basename with a URL-derived fallback, treating `-1` as an unknown size, and carrying referer, MIME type, `byExtensionId`, and incognito state. The shared controller still sends the capture before canceling and erasing the browser download, so a failure leaves the browser file intact and a fast download that finishes first is reported honestly as a race rather than prevented. The bounded takeover-failure record is kept in session storage for Firefox and rehydrated on event-page startup so the popup retry path survives suspension; Chromium keeps the equivalent record in memory. Both clear it after a successful takeover or same-URL prefill retry, and neither writes it to persistent storage. Resource metadata remains session-only and bounded.

The build writes `extension/dist/chromium` and `extension/dist/firefox` from separate manifests with separate origin substitution, and the test build keeps writing only the Chromium test package. An exact `web-ext` dependency and `npm run lint:extension:firefox` lint the built Firefox package, and the frontend CI job runs the Firefox build and lint before the Chromium test build and extension Playwright suite. No new hosted CI run is claimed.

## Alternatives considered

- Duplicating the controller inside a Firefox package was declined because two copies of takeover, prefill, pairing, and resource bounds would drift and would double the rules to test.
- Adopting a WebExtension polyfill was declined because the surface needed is small and a typed local adapter keeps the dependency and its promise and callback differences explicit.
- Emulating `onDeterminingFilename` for Firefox was declined because Firefox does not expose an equivalent pause point; using `downloads.onCreated` and documenting the race is the honest boundary.
- Accepting any `moz-extension://` string or a case-insensitive UUID was declined because it would widen the trusted origin boundary that protects a bearer token.
- Persisting the Firefox takeover-failure record in local storage was declined because it is UI state, not durable pairing or settings data, and session scope preserves privacy and survives the suspension it must survive.
- Passing the absolute browser path to the desktop and relying on later sanitization was declined because it leaks a local path into the capture protocol and bypasses the filename bound.
- Adding Firefox Android support, AMO publishing, or signing in this change was declined because the capability targets desktop Firefox and the release path is separate work.

## Consequences

The shared controller and adapter are the single behavior source for both packages, and Firefox-specific code is limited to its manifest, adapter, download mapping, and event installation. Chromium keeps its service-worker listeners and `onDeterminingFilename`, and its existing pairing rows, capture behavior, and resource bounds are unchanged; Firefox rows add their own exact origins without affecting Chromium clients.

The capture protocol, pairing schema, task-store schema, settings schema, and task semantics are unchanged. Existing Chromium pairings remain valid, Firefox pairings use the same routes and limits, and rolling back the Firefox package leaves Chromium, tasks, completed files, and settings usable.

Security is unchanged in kind: no new permissions, cookies, native messaging, remote code, or telemetry, and the Firefox manifest declares no data collection. The origin predicate, token binding, admission order, limits, and error codes stay the contract that later extension work must preserve. The `web-ext` lint result is zero errors and one advisory warning that the no-data-collection key is unsupported on Firefox for Android; the warning is accepted because Android is out of scope and the desktop floor stays 140.0.

Automated evidence covers the Rust origin and capture suites, the shared adapter and Firefox mapping unit tests, both production builds, the Chromium test build, the Firefox lint, and the existing frontend, extension, and Playwright suites. No real Firefox browser interaction, event-page suspension, Windows Firefox check, or manual Chrome or Edge record has been performed, so the browser-capture and Firefox exits stay blocked.

- The context indexer discovers only TypeScript modules; Rust navigation entries and the semantic responsibility of generated feature entries stay hand-maintained after a rebuild.
- Automated, mock-service, and lint results never replace a manual Firefox or Windows record, and a pending manual item is never recorded as passed.
- Firefox for Android, publishing, signing, HLS and DASH download, video parsing, cookie access, and telemetry remain outside this decision.
