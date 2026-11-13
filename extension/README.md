# OneGet browser extension

The Chromium extension connects Chrome and Edge to the OneGet desktop app. It
observes page resources, lists them in the popup, sends prefill captures, and
optionally takes over matching browser downloads.

## Layout

- `shared/` holds framework-free protocol, classification, filtering, settings,
  storage, page ownership, and bilingual message logic.
- `chromium/` holds the MV3 manifest, background service worker, isolated
  content script, MAIN-world fetch and XHR observer, and the video overlay.
- `popup/` holds the popup document, controller, and styles.
- `build.mjs` bundles the runtime and writes an unpacked extension.
- `tests/unit/` covers the shared modules and the background and popup
  controllers.

## Build

```sh
npm run build:extension
npm run build:extension:test
```

The production build writes `extension/dist/chromium/` and targets
`http://127.0.0.1:37652`. Set `ONEGET_API_ORIGIN` to another loopback HTTP
origin to point the build at a different port; non-loopback, credentialed, and
path-bearing values are rejected. The test build writes
`extension/dist/chromium-test/` with its own loopback test origin.

Load the unpacked directory from `chrome://extensions` or
`edge://extensions` with developer mode enabled. The app must be running before
pairing.

## Runtime behavior

The background worker reads the stored token and settings for every takeover
decision, so saving settings applies to the next browser download without a
browser restart. A matching takeover posts one capture, then cancels and erases
the browser download only after the app confirms the task. Duplicate source
identifiers return the existing task and never cancel twice. A 401 clears the
local token and asks the user to pair again; a 409 asks the user to update
OneGet or the extension.

Resource metadata is collected from `webRequest`, the MAIN-world fetch and XHR
wrappers, and page media elements. The extension never reads response bodies,
cookies, or authorization values. Resources are deduplicated, transport
segments are filtered, HLS and DASH manifests stay visible with a disabled send
action, and the per-tab store is bounded. The resource list lives in
`chrome.storage.session` only; the pairing token and settings use
`chrome.storage.local`.

The overlay runs in the top frame only, reuses a single button for the hovered
video, and explains that video download is not available yet. Activating it
creates no task.

## Permissions and privacy

The manifest requests `webRequest`, `downloads`, `storage`, `tabs`, and
`webNavigation`, with host permissions for HTTP(S) pages and the loopback API.
It requests no cookies, no native messaging, no remote code, and no telemetry.
The extension-page CSP allows only bundled scripts (`script-src 'self'`), and
the build copies the local OneGet icons into the package. Popup URLs are
redacted: credentials, query strings, and fragments are never displayed.

## Checks

```sh
npm run test:extension
npm run typecheck:extension
npm run build:extension:test
npm run test:e2e:extension
```

`npm run test:e2e:extension` rebuilds `extension/dist/chromium-test/` and runs
the Playwright suite in `extension/tests/e2e`. The suite starts its own fixture
page and mock API on `127.0.0.1` and launches a persistent Chromium profile with
the test build loaded; it does not contact a public host and does not use the
main application's Playwright projects.

Playwright's default download interception can suppress
`downloads.onDeterminingFilename`. The extension suite restores browser-managed
downloads through the DevTools protocol so the takeover scenario exercises the
real event, and the scenario is skipped only when the API itself is
unavailable. Headless Chromium can still differ from a user's browser, so real
takeover, pairing, sniffing, HLS filtering, and unpaired or revoked behavior
must be verified manually in current stable Chrome and Edge against the running
desktop app. Record the browser versions, operating system, build, steps, and
results in the verification record, mark unperformed items pending, and never
claim automated takeover evidence for a skipped case.

