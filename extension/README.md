# OneGet browser extension

The browser extension connects Chromium browsers (Chrome and Edge) and Firefox
to the OneGet desktop app. It observes page resources, lists them in the popup,
sends prefill captures, and optionally takes over matching browser downloads.

## Layout

- `shared/` holds the browser-agnostic background controller, typed browser
  adapter, isolated content script, MAIN-world fetch and XHR observer, overlay
  styles, and framework-free protocol, classification, filtering, settings,
  storage, page ownership, and bilingual message logic.
- `chromium/` holds the MV3 manifest and the thin service-worker installer that
  keeps the Chromium event wiring and `onDeterminingFilename`.
- `firefox/` holds the MV3 manifest and the event-page adapter that registers
  Firefox listeners synchronously and uses `downloads.onCreated`.
- `popup/` holds the popup document, controller, and styles.
- `build.mjs` bundles the runtime and writes an unpacked extension for each
  browser target.
- `tests/unit/` covers the shared modules and the background and popup
  controllers.

## Build

```sh
npm run build:extension
npm run build:extension:test
```

The production build writes `extension/dist/chromium/` and
`extension/dist/firefox/` with separate manifests and targets
`http://127.0.0.1:37652`. Set `ONEGET_API_ORIGIN` to another loopback HTTP
origin to point the build at a different port; non-loopback, credentialed, and
path-bearing values are rejected. The test build writes only
`extension/dist/chromium-test/` with its own loopback test origin.

Load the Chromium package from `chrome://extensions` or `edge://extensions` with
developer mode enabled. Load the Firefox package from
`about:debugging#/runtime/this-firefox` with **Load Temporary Add-on** and select
`extension/dist/firefox/manifest.json`; grant the requested site and download
permissions when Firefox prompts. The Firefox package requires desktop Firefox
140 or later, declares no data collection, and is not supported on Firefox for
Android. The app must be running before pairing.

## Runtime behavior

The background runtime reads the stored token and settings for every takeover
decision, so saving settings applies to the next browser download without a
browser restart. A matching takeover posts one capture, then cancels and erases
the browser download only after the app confirms the task. Duplicate source
identifiers return the existing task and never cancel twice. A 401 clears the
local token and asks the user to pair again; a 409 asks the user to update
OneGet or the extension. Chromium evaluates downloads in
`downloads.onDeterminingFilename`; Firefox uses `downloads.onCreated`, reduces
an absolute filename to a safe basename with a URL fallback, treats a `-1` size
as unknown, and can miss a download that finishes before cancellation.

Resource metadata is collected from `webRequest`, the MAIN-world fetch and XHR
wrappers, and page media elements. The extension never reads response bodies,
cookies, or authorization values. Resources are deduplicated, transport
segments are filtered, HLS and DASH manifests stay visible with a disabled send
action, and the per-tab store is bounded. The resource list lives in the
session storage area only; the pairing token and settings use the local
storage area.

The overlay runs in the top frame only, reuses a single button for the hovered
video, and explains that video download is not available yet. Activating it
creates no task.

## Permissions and privacy

Both manifests request `webRequest`, `downloads`, `storage`, `tabs`, and
`webNavigation`, with host permissions for HTTP(S) pages and the loopback API.
Neither requests cookies, native messaging, remote code, or telemetry, and the
Firefox manifest declares no data collection.
The extension-page CSP allows only bundled scripts (`script-src 'self'`), and
the build copies the local OneGet icons into the package. Popup URLs are
redacted: credentials, query strings, and fragments are never displayed.

## Checks

```sh
npm run test:extension
npm run typecheck:extension
npm run build:extension
npm run lint:extension:firefox
npm run build:extension:test
npm run test:e2e:extension
```

`npm run lint:extension:firefox` lints the built Firefox package with an exact
`web-ext` pin and requires the production build to exist. It reports zero
errors and one advisory warning that the no-data-collection declaration is
unsupported on Firefox for Android; that platform is out of scope and the
desktop floor stays 140.0. `npm run test:e2e:extension`
rebuilds `extension/dist/chromium-test/` and runs
the Playwright suite in `extension/tests/e2e`; the Firefox package has no
automated browser suite, so its manual record stays pending until performed. The suite starts its own fixture
page and mock API on `127.0.0.1` and launches a persistent Chromium profile with
the test build loaded; it does not contact a public host and does not use the
main application's Playwright projects.

Playwright's default download interception can suppress
`downloads.onDeterminingFilename`. The extension suite restores browser-managed
downloads through the DevTools protocol so the takeover scenario exercises the
real event, and the scenario is skipped only when the API itself is
unavailable. Headless Chromium can still differ from a user's browser, so real
takeover, pairing, sniffing, HLS filtering, and unpaired or revoked behavior
must be verified manually in current stable Chrome, Edge, and Firefox against
the running desktop app, including a Firefox event-page suspension after a
failed takeover. Record the browser versions, operating system, build, steps, and
results in the verification record, mark unperformed items pending, and never
claim automated takeover evidence for a skipped case.

